use super::*;
use crate::storage::Chunk;
use crate::testing::TestWorld;
use crate::World;
use mchprs_blocks::generic::{default_state, with_property};
use mchprs_blocks::BlockPos;
use std::time::{Duration, Instant};

fn id(name: &str) -> u32 {
    default_state(&format!("minecraft:{name}")).unwrap()
}

/// A world of 2x2 chunks that is three sections high, full of air
fn world() -> (TestWorld, LightEngine) {
    (TestWorld::new(2, 3, 2), LightEngine::new())
}

fn update(world: &mut TestWorld, engine: &mut LightEngine) {
    for chunk in &mut world.chunks {
        engine.collect_pending(chunk);
    }
    assert!(engine.run(world, None));
}

fn set(world: &mut TestWorld, engine: &mut LightEngine, x: i32, y: i32, z: i32, block: u32) {
    world.set_block_raw(BlockPos::new(x, y, z), block);
    update(world, engine);
}

fn chunk_at(world: &TestWorld, x: i32, z: i32) -> &Chunk {
    world.get_chunk(x >> 4, z >> 4).unwrap()
}

fn sky(world: &TestWorld, x: i32, y: i32, z: i32) -> u8 {
    chunk_at(world, x, z)
        .light
        .sky_light((x & 15) as u32, y as u32, (z & 15) as u32)
}

fn block(world: &TestWorld, x: i32, y: i32, z: i32) -> u8 {
    chunk_at(world, x, z)
        .light
        .block_light((x & 15) as u32, y as u32, (z & 15) as u32)
}

fn fill(world: &mut TestWorld, x: i32, y: i32, z: i32, size: (i32, i32, i32), state: u32) {
    for dx in 0..size.0 {
        for dy in 0..size.1 {
            for dz in 0..size.2 {
                world.set_block_raw(BlockPos::new(x + dx, y + dy, z + dz), state);
            }
        }
    }
}

#[test]
fn empty_world_is_lit_by_the_sky() {
    let (world, _) = world();
    assert_eq!(sky(&world, 5, 0, 5), 15);
    assert_eq!(sky(&world, 31, 47, 31), 15);
    assert_eq!(block(&world, 5, 5, 5), 0);
}

#[test]
fn torch_lights_and_unlights() {
    let (mut world, mut engine) = world();
    set(&mut world, &mut engine, 8, 5, 8, id("torch"));
    assert_eq!(block(&world, 8, 5, 8), 14);
    assert_eq!(block(&world, 9, 5, 8), 13);
    assert_eq!(block(&world, 8, 8, 8), 11);
    assert_eq!(block(&world, 10, 7, 9), 14 - 5);
    // Across the chunk border at x = 16
    assert_eq!(block(&world, 16, 5, 8), 14 - 8);
    assert_eq!(block(&world, 8, 5, 22), 0);

    set(&mut world, &mut engine, 8, 5, 8, 0);
    for x in 0..32 {
        for y in 0..48 {
            assert_eq!(block(&world, x, y, 8), 0, "{x} {y}");
        }
    }
}

#[test]
fn light_crosses_sections() {
    let (mut world, mut engine) = world();
    set(&mut world, &mut engine, 3, 15, 3, id("glowstone"));
    assert_eq!(block(&world, 3, 16, 3), 14);
    assert_eq!(block(&world, 3, 20, 3), 10);
    assert_eq!(block(&world, 3, 30, 3), 0);
    assert_eq!(block(&world, 3, 3, 3), 15 - 12);
}

#[test]
fn walls_block_light_and_opening_them_lets_it_through() {
    let (mut world, mut engine) = world();
    // A hollow stone cube with a room inside
    fill(&mut world, 6, 4, 6, (7, 7, 7), id("stone"));
    fill(&mut world, 7, 5, 7, (5, 5, 5), 0);
    update(&mut world, &mut engine);
    set(&mut world, &mut engine, 9, 6, 9, id("torch"));
    assert_eq!(block(&world, 9, 7, 9), 13);
    assert_eq!(block(&world, 11, 7, 9), 11);
    assert_eq!(block(&world, 4, 7, 9), 0);
    // Opening a hole in the wall
    set(&mut world, &mut engine, 6, 7, 9, 0);
    assert_eq!(block(&world, 7, 7, 9), 11);
    assert_eq!(block(&world, 6, 7, 9), 10);
    assert_eq!(block(&world, 5, 7, 9), 9);
    assert_eq!(block(&world, 1, 7, 9), 5);
    set(&mut world, &mut engine, 6, 7, 9, id("stone"));
    assert_eq!(block(&world, 5, 7, 9), 0);
    assert_eq!(block(&world, 7, 7, 9), 11);
}

#[test]
fn lamp_emits_when_lit() {
    let (mut world, mut engine) = world();
    let lamp = id("redstone_lamp");
    set(&mut world, &mut engine, 8, 5, 8, lamp);
    assert_eq!(block(&world, 8, 5, 8), 0);
    set(
        &mut world,
        &mut engine,
        8,
        5,
        8,
        with_property(lamp, "lit", "true"),
    );
    assert_eq!(block(&world, 8, 5, 8), 15);
    assert_eq!(block(&world, 8, 6, 8), 14);
    set(&mut world, &mut engine, 8, 5, 8, lamp);
    assert_eq!(block(&world, 8, 6, 8), 0);
}

#[test]
fn roof_shades_the_sky() {
    let (mut world, mut engine) = world();
    // A roof over half of the world
    fill(&mut world, 0, 30, 0, (16, 1, 32), id("stone"));
    update(&mut world, &mut engine);
    assert_eq!(sky(&world, 8, 31, 8), 15);
    // Below the roof the light only comes in from the side
    assert_eq!(sky(&world, 15, 29, 8), 14);
    assert_eq!(sky(&world, 14, 29, 8), 13);
    assert_eq!(sky(&world, 15, 0, 8), 14);
    assert_eq!(sky(&world, 0, 29, 8), 0);
    assert_eq!(sky(&world, 16, 29, 8), 15);
    // Removing a roof block lets a beam of sky light in, and it spreads
    set(&mut world, &mut engine, 4, 30, 8, 0);
    assert_eq!(sky(&world, 4, 29, 8), 15);
    assert_eq!(sky(&world, 4, 0, 8), 15);
    assert_eq!(sky(&world, 5, 10, 8), 14);
    assert_eq!(sky(&world, 6, 10, 9), 12);
    // And closing it again
    set(&mut world, &mut engine, 4, 30, 8, id("stone"));
    // Only the light that comes in from the open side remains
    assert_eq!(sky(&world, 4, 29, 8), 3);
    assert_eq!(sky(&world, 4, 0, 8), 3);
    assert_eq!(sky(&world, 15, 29, 8), 14);
}

#[test]
fn glass_lets_the_sky_through() {
    let (mut world, mut engine) = world();
    fill(&mut world, 0, 30, 0, (32, 1, 32), id("glass"));
    update(&mut world, &mut engine);
    assert_eq!(sky(&world, 10, 29, 10), 15);
    assert_eq!(sky(&world, 10, 0, 10), 15);

    // Water takes one level per block
    fill(&mut world, 0, 30, 0, (32, 1, 32), id("water"));
    update(&mut world, &mut engine);
    assert_eq!(sky(&world, 10, 30, 10), 14);
    assert_eq!(sky(&world, 10, 29, 10), 13);
}

#[test]
fn sky_light_flows_around_corners() {
    let (mut world, mut engine) = world();
    fill(&mut world, 0, 0, 0, (32, 20, 32), id("stone"));
    // A shaft down from the surface and a tunnel to the side at the bottom
    fill(&mut world, 10, 0, 10, (3, 20, 3), 0);
    fill(&mut world, 10, 4, 12, (6, 2, 1), 0);
    update(&mut world, &mut engine);
    assert_eq!(sky(&world, 11, 17, 11), 15);
    assert_eq!(sky(&world, 11, 5, 11), 15);
    assert_eq!(sky(&world, 12, 5, 12), 15);
    assert_eq!(sky(&world, 13, 5, 12), 14);
    assert_eq!(sky(&world, 15, 5, 12), 12);
    // Covering the shaft turns it off
    fill(&mut world, 10, 18, 10, (3, 1, 3), id("stone"));
    update(&mut world, &mut engine);
    assert_eq!(sky(&world, 11, 17, 11), 0);
    assert_eq!(sky(&world, 15, 5, 12), 0);
}

struct Rng(u64);

impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

fn assert_same_light(a: &TestWorld, b: &TestWorld) {
    for x in 0..32 {
        for z in 0..32 {
            for y in 0..48 {
                assert_eq!(sky(a, x, y, z), sky(b, x, y, z), "sky {x} {y} {z}");
                assert_eq!(block(a, x, y, z), block(b, x, y, z), "block {x} {y} {z}");
            }
        }
    }
}

/// Light that was updated step by step must be the same as light calculated from scratch
#[test]
fn incremental_updates_match_initial_calculation() {
    let palette = [
        0,
        id("stone"),
        id("stone"),
        id("glass"),
        id("torch"),
        id("water"),
        id("oak_leaves"),
        id("glowstone"),
        id("oak_slab"),
        id("oak_stairs"),
    ];
    let mut rng = Rng(7);
    let (mut incremental, mut engine) = world();
    for round in 0..6 {
        for _ in 0..400 {
            let (x, y, z) = (
                rng.next(32) as i32,
                rng.next(48) as i32,
                rng.next(32) as i32,
            );
            // Mostly air above, mostly solid below
            let state = if y > 20 && rng.next(4) != 0 {
                0
            } else {
                palette[rng.next(palette.len() as u64) as usize]
            };
            incremental.set_block_raw(BlockPos::new(x, y, z), state);
        }
        if round % 2 == 0 {
            // Roofs and floors
            let y = 10 + rng.next(30) as i32;
            let state = palette[1 + rng.next(3) as usize];
            fill(&mut incremental, 0, y, 0, (32, 1, 32), state);
        }
        update(&mut incremental, &mut engine);

        let mut scratch = world().0;
        for x in 0..32 {
            for z in 0..32 {
                for y in 0..48 {
                    let state = incremental.get_block_raw(BlockPos::new(x, y, z));
                    scratch.set_block_raw(BlockPos::new(x, y, z), state);
                }
            }
        }
        let coords: Vec<_> = scratch.chunks.iter().map(|c| (c.x, c.z)).collect();
        LightEngine::new().initialize(&mut scratch, &coords);
        assert_same_light(&incremental, &scratch);
    }
}

#[test]
fn budgeted_run_finishes_eventually() {
    let (mut world, mut engine) = world();
    for i in 0..30 {
        world.set_block_raw(BlockPos::new(i, 20, 8), id("glowstone"));
    }
    for chunk in &mut world.chunks {
        engine.collect_pending(chunk);
    }
    let mut runs = 0;
    while !engine.run(&mut world, Some(Duration::ZERO)) {
        runs += 1;
        assert!(runs < 100_000);
    }
    assert!(runs > 0);
    assert_eq!(block(&world, 10, 20, 8), 15);
    assert_eq!(block(&world, 10, 21, 9), 13);
}

#[test]
fn dirty_sections_are_tracked() {
    let (mut world, mut engine) = world();
    set(&mut world, &mut engine, 3, 15, 3, id("glowstone"));
    let light = &mut world.get_chunk_mut(0, 0).unwrap().light;
    let (_, block_dirty) = light.take_dirty().unwrap();
    // Sections 0 and 1 of the chunk are the light sections 1 and 2
    assert_eq!(block_dirty & 0b110, 0b110);
    assert!(light.take_dirty().is_none());
}

/// Run with `cargo test --release -p mchprs_world -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_initial_plot_light() {
    const WIDTH: i32 = 32;
    const SECTIONS: i32 = 24;
    let mut world = TestWorld::new(WIDTH, SECTIONS, WIDTH);
    let stone = id("stone");
    let dirt = id("dirt");
    let grass = id("grass_block");
    let torch = id("torch");
    let mut rng = Rng(1);
    // A flat world with some torches and a few caves
    for x in 0..WIDTH * 16 {
        for z in 0..WIDTH * 16 {
            for y in 0..60 {
                world.set_block_raw(BlockPos::new(x, y, z), if y < 50 { stone } else { dirt });
            }
            world.set_block_raw(BlockPos::new(x, 60, z), grass);
            if rng.next(200) == 0 {
                world.set_block_raw(BlockPos::new(x, 61, z), torch);
            }
        }
    }
    for _ in 0..200 {
        let (x, y, z) = (
            rng.next(400) as i32 + 20,
            rng.next(40) as i32,
            rng.next(400) as i32 + 20,
        );
        fill(&mut world, x, y, z, (6, 4, 6), 0);
    }
    let coords: Vec<_> = world.chunks.iter().map(|c| (c.x, c.z)).collect();
    let mut engine = LightEngine::new();
    let start = Instant::now();
    engine.initialize(&mut world, &coords);
    println!("initial light of a full plot: {:?}", start.elapsed());
    assert_eq!(sky(&world, 100, 70, 100), 15);

    let start = Instant::now();
    set(&mut world, &mut engine, 100, 80, 100, id("glowstone"));
    set(&mut world, &mut engine, 100, 80, 100, 0);
    println!("incremental update of two blocks: {:?}", start.elapsed());
}

/// Prints the light packets as hex, to be decoded with the vanilla classes (`Decode.java`).
/// Run with `cargo test -p mchprs_world --features networking dump_light_packets -- --ignored --nocapture`
#[cfg(feature = "networking")]
#[test]
#[ignore]
fn dump_light_packets() {
    let (mut world, mut engine) = world();
    fill(&mut world, 0, 0, 0, (16, 20, 16), id("stone"));
    fill(&mut world, 4, 20, 4, (3, 1, 3), id("glass"));
    update(&mut world, &mut engine);
    set(&mut world, &mut engine, 8, 25, 8, id("torch"));
    let chunk = world.get_chunk_mut(0, 0).unwrap();
    let hex = |packet: mchprs_network::packets::PacketEncoder| {
        let mut bytes = Vec::new();
        packet.write_uncompressed(&mut bytes).unwrap();
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    println!("LIGHT {}", hex(chunk.encode_light_update().unwrap()));
    println!("CHUNK {}", hex(chunk.encode_packet()));
}
