//! Sky light and block light.
//!
//! The light of a chunk lives in [`ChunkLight`], next to the blocks. Changing a block records the
//! position in the chunk (see `Chunk::set_block`), and the [`LightEngine`] later spreads the
//! effect of those changes through the world, one breadth first pass for removing light and one
//! for adding it. The algorithm follows the one of the vanilla server.
//!
//! The light of a chunk covers the sections of the chunk and one extra section below and above,
//! like the protocol. Light is only calculated inside the world the chunks belong to: chunks
//! that don't exist (for plots, everything outside the plot) neither receive nor pass on light.
//! The extra section above the world has sky light 15 and the one below has none, neither is
//! ever updated.

mod engine;
mod init;
mod layer;

pub use engine::LightEngine;
pub use layer::Nibbles;

use cinnabar_blocks::generic::{light_emission, light_occluding_faces, light_opacity};

pub const MAX_LIGHT: u8 = 15;

/// The order of the bits in the masks of occluding faces and of the directions in the engine
pub(crate) const DIRECTIONS: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];
pub(crate) const DOWN: usize = 0;
pub(crate) const UP: usize = 1;
pub(crate) const OPPOSITE: [usize; 6] = [1, 0, 3, 2, 5, 4];

/// Whether the light properties of the two states differ in a way the engine has to react to
pub(crate) fn light_changes(old: u32, new: u32) -> bool {
    old != new
        && (light_emission(old) != light_emission(new)
            || light_opacity(old) != light_opacity(new)
            || light_occluding_faces(old) | light_occluding_faces(new) != 0)
}

/// Whether sky light can't fall from the cell above `top` into the cell `bottom`
pub(crate) fn edge_occluded(top: u32, bottom: u32) -> bool {
    light_opacity(bottom) != 0
        || light_occluding_faces(top) & 1 << DOWN != 0
        || light_occluding_faces(bottom) & 1 << UP != 0
}

/// The light data of a chunk
#[derive(Clone)]
pub struct ChunkLight {
    /// The sky light of every light section, the first one is below the chunk
    pub sky: Vec<Nibbles>,
    pub block: Vec<Nibbles>,
    num_sections: usize,
    /// For every column (index `x + z * 16`) the y of the lowest cell that sky light falls into
    /// without having been reduced. Every cell from there up has sky light 15. It is -1 if the
    /// sky is open down to the bottom of the world and the height of the world if the topmost
    /// block is opaque.
    sources: Box<[i16; 256]>,
    /// Positions in the chunk (`y << 8 | z << 4 | x`) where the light properties of the block
    /// changed
    pending: Vec<u32>,
    /// Bit n is set when the light section n changed since the last call to `take_dirty`
    dirty_sky: u128,
    dirty_block: u128,
}

impl ChunkLight {
    /// The light of a chunk of the given number of sections without any blocks
    pub fn new(num_sections: usize) -> ChunkLight {
        assert!(num_sections + 2 <= 128, "too many sections");
        let mut sky = vec![Nibbles::Uniform(MAX_LIGHT); num_sections + 2];
        sky[0] = Nibbles::Uniform(0);
        ChunkLight {
            sky,
            block: vec![Nibbles::Uniform(0); num_sections + 2],
            num_sections,
            sources: Box::new([-1; 256]),
            pending: Vec::new(),
            dirty_sky: 0,
            dirty_block: 0,
        }
    }

    pub fn num_sections(&self) -> usize {
        self.num_sections
    }

    pub fn sky_light(&self, x: u32, y: u32, z: u32) -> u8 {
        self.sky[(y >> 4) as usize + 1].get(x, y & 15, z)
    }

    pub fn block_light(&self, x: u32, y: u32, z: u32) -> u8 {
        self.block[(y >> 4) as usize + 1].get(x, y & 15, z)
    }

    pub(crate) fn set_sky_light(&mut self, x: u32, y: u32, z: u32, level: u8) {
        let section = (y >> 4) as usize + 1;
        if self.sky[section].set(x, y & 15, z, level) {
            self.dirty_sky |= 1 << section;
        }
    }

    pub(crate) fn set_block_light(&mut self, x: u32, y: u32, z: u32, level: u8) {
        let section = (y >> 4) as usize + 1;
        if self.block[section].set(x, y & 15, z, level) {
            self.dirty_block |= 1 << section;
        }
    }

    /// The y of the lowest sky source in a column, see `sources`
    pub fn source_y(&self, x: u32, z: u32) -> i32 {
        self.sources[(x + z * 16) as usize] as i32
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(crate) fn take_pending(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pending)
    }

    /// The sections (as bit masks over the light sections) whose sky light and block light
    /// changed since the last call
    pub fn take_dirty(&mut self) -> Option<(u128, u128)> {
        let dirty = (self.dirty_sky, self.dirty_block);
        self.dirty_sky = 0;
        self.dirty_block = 0;
        (dirty != (0, 0)).then_some(dirty)
    }

    /// Called when the block at the position in the chunk has changed. `state_at` returns the
    /// block states of the column at the position, which must already be updated.
    pub(crate) fn block_changed(
        &mut self,
        (x, y, z): (u32, u32, u32),
        old: u32,
        new: u32,
        state_at: impl Fn(i32) -> u32,
    ) {
        if !light_changes(old, new) {
            return;
        }
        self.pending.push(y << 8 | z << 4 | x);
        self.update_source(x, y as i32, z, &state_at);
    }

    /// Keeps the lowest source of the column up to date after the block at y changed
    fn update_source(&mut self, x: u32, y: i32, z: u32, state_at: &impl Fn(i32) -> u32) {
        let idx = (x + z * 16) as usize;
        let current = self.sources[idx] as i32;
        if y + 1 < current {
            return;
        }
        // The edge above the cell and the edge below it
        for edge in [y + 1, y] {
            let occluded = edge_occluded(state_at(edge), state_at(edge - 1));
            if occluded && edge > current {
                self.sources[idx] = edge as i16;
                return;
            }
            if !occluded && edge == current {
                let mut lowest = -1;
                for below in (0..edge).rev() {
                    if edge_occluded(state_at(below + 1), state_at(below)) {
                        lowest = below + 1;
                        break;
                    }
                }
                self.sources[idx] = lowest as i16;
                return;
            }
        }
    }

    #[cfg(feature = "networking")]
    pub fn encode(
        &self,
        sky_filter: Option<u128>,
        block_filter: Option<u128>,
    ) -> cinnabar_network::packets::clientbound::LightData {
        use cinnabar_network::packets::clientbound::{LightData, LightSection};
        let convert = |layers: &[Nibbles], filter: Option<u128>| {
            layers
                .iter()
                .enumerate()
                .map(|(i, layer)| {
                    if filter.is_some_and(|mask| mask >> i & 1 == 0) {
                        LightSection::Unchanged
                    } else if layer.is_zero() {
                        LightSection::Empty
                    } else {
                        LightSection::Data(layer.to_bytes())
                    }
                })
                .collect()
        };
        LightData {
            sky: convert(&self.sky, sky_filter),
            block: convert(&self.block, block_filter),
        }
    }
}

#[cfg(test)]
mod tests;
