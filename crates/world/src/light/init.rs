//! Calculating the light of chunks from scratch

use super::engine::{Entry, Kind, LightEngine, Node};
use super::{edge_occluded, ChunkLight, Nibbles, DIRECTIONS, MAX_LIGHT};
use crate::storage::Chunk;
use crate::World;
use cinnabar_blocks::generic::light_emission;

/// Index of the directions north, south, west and east in `DIRECTIONS`
const SIDES: [usize; 4] = [2, 3, 4, 5];
const DOWN_BIT: u8 = 1 << super::DOWN;

impl Chunk {
    /// Calculates the sky sources and the sky light of the chunk from its blocks and removes
    /// the block light. This is complete for chunks that have no neighbors, otherwise the
    /// light still needs to flow in and out of the chunk, see `LightEngine::initialize`.
    pub fn reset_light(&mut self) {
        let num_sections = self.sections.len();
        let mut light = ChunkLight::new(num_sections);
        let mut max_lowest = -1;
        for z in 0..16 {
            for x in 0..16 {
                let lowest = self.find_lowest_source(x, z);
                light.sources[(x + z * 16) as usize] = lowest as i16;
                max_lowest = max_lowest.max(lowest);
            }
        }

        for section in 0..num_sections {
            let section_y = section as i32 * 16;
            if section_y >= max_lowest {
                // Open to the sky everywhere
                continue;
            }
            let mut layer = Nibbles::Uniform(0);
            for z in 0..16 {
                for x in 0..16 {
                    let lowest = light.sources[(x + z * 16) as usize] as i32;
                    for y in lowest.max(section_y)..section_y + 16 {
                        layer.set(x, (y - section_y) as u32, z, MAX_LIGHT);
                    }
                }
            }
            light.sky[section + 1] = layer;
        }
        self.light = light;
    }

    /// The y of the lowest cell in the column that sky light falls into, see
    /// `ChunkLight::source_y`
    fn find_lowest_source(&self, x: u32, z: u32) -> i32 {
        let mut above = 0;
        let mut y = self.sections.len() as i32 * 16 - 1;
        while y >= 0 {
            let section = &self.sections[(y >> 4) as usize];
            if section.block_count() == 0 {
                above = 0;
                y = (y & !15) - 1;
                continue;
            }
            let state = section.get_block(x, (y & 15) as u32, z);
            if edge_occluded(above, state) {
                return y + 1;
            }
            above = state;
            y -= 1;
        }
        -1
    }
}

impl LightEngine {
    /// Calculates all light of the chunks with the given coordinates from scratch. Light
    /// that came from a chunk and went through chunks not in the list is not removed
    /// properly, so this is meant for loading whole worlds.
    pub fn initialize<W: World>(&mut self, world: &mut W, chunks: &[(i32, i32)]) {
        for &(x, z) in chunks {
            if let Some(chunk) = world.get_chunk_mut(x, z) {
                chunk.reset_light();
            }
        }
        for &(x, z) in chunks {
            self.seed_sky(world, x, z);
            self.seed_block(world, x, z);
        }
        self.run(world, None);
        for &(x, z) in chunks {
            if let Some(chunk) = world.get_chunk_mut(x, z) {
                chunk.light.take_dirty();
            }
        }
    }

    /// Queues the sky sources that have neighbors which sky light has to flow into: the
    /// block below the lowest source and the blocks next to columns with a higher lowest
    /// source.
    fn seed_sky<W: World>(&mut self, world: &W, chunk_x: i32, chunk_z: i32) {
        let Some(chunk) = world.get_chunk(chunk_x, chunk_z) else {
            return;
        };
        let height = chunk.light.num_sections() as i32 * 16;
        let neighbors = [(0, -1), (0, 1), (-1, 0), (1, 0)]
            .map(|(dx, dz)| world.get_chunk(chunk_x + dx, chunk_z + dz));
        let source_y = |chunk: &Chunk, x: i32, z: i32| chunk.light.source_y(x as u32, z as u32);
        for z in 0..16i32 {
            for x in 0..16i32 {
                let lowest = source_y(chunk, x, z);
                // The lowest source of the column next to this one, in the order of `SIDES`
                let neighbor_lowest =
                    [(x, z - 1), (x, z + 1), (x - 1, z), (x + 1, z)].map(|(nx, nz)| {
                        let (neighbor, nx, nz) = match (nx, nz) {
                            (_, -1) => (neighbors[0], nx, 15),
                            (_, 16) => (neighbors[1], nx, 0),
                            (-1, _) => (neighbors[2], 15, nz),
                            (16, _) => (neighbors[3], 0, nz),
                            _ => (Some(chunk), nx, nz),
                        };
                        neighbor.map_or(i32::MIN, |chunk| source_y(chunk, nx, nz))
                    });
                let highest = neighbor_lowest.into_iter().max().unwrap();
                let end = height.min(highest.max(lowest + 1));
                for y in lowest.max(0)..end {
                    let mut dirs = 0;
                    if y == lowest {
                        dirs |= DOWN_BIT;
                    }
                    for (side, neighbor_lowest) in SIDES.iter().zip(neighbor_lowest) {
                        if y < neighbor_lowest {
                            dirs |= 1 << side;
                        }
                    }
                    if dirs != 0 {
                        let node = Node {
                            x: chunk_x * 16 + x,
                            y,
                            z: chunk_z * 16 + z,
                        };
                        self.enqueue_increase(Kind::Sky, Entry::new(node, MAX_LIGHT, dirs));
                    }
                }
            }
        }
        debug_assert!(SIDES.iter().all(|&side| DIRECTIONS[side].1 == 0));
    }

    fn seed_block<W: World>(&mut self, world: &W, chunk_x: i32, chunk_z: i32) {
        let Some(chunk) = world.get_chunk(chunk_x, chunk_z) else {
            return;
        };
        for (section_idx, section) in chunk.sections.iter().enumerate() {
            if section.block_count() == 0 || !section.any_state(|state| light_emission(state) > 0) {
                continue;
            }
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        let emission = light_emission(section.get_block(x, y, z));
                        if emission > 0 {
                            let node = Node {
                                x: chunk_x * 16 + x as i32,
                                y: section_idx as i32 * 16 + y as i32,
                                z: chunk_z * 16 + z as i32,
                            };
                            let mut entry = Entry::new(node, emission, 0b11_1111);
                            entry.from_emission = true;
                            self.enqueue_increase(Kind::Block, entry);
                        }
                    }
                }
            }
        }
    }
}
