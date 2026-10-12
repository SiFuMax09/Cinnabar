//! Benchmarks for the bulk operations. Run with
//! `cargo test --release -p mchprs_core worldedit::bench -- --ignored --nocapture`

use super::*;
use mchprs_schematic::{create_clipboard, paste_clipboard};
use std::time::Instant;

fn time<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    println!("{:<28} {:?}", name, start.elapsed());
    result
}

#[test]
#[ignore]
fn baseline_old_pipeline() {
    let mut world = PlotWorld::empty_for_test(0, 0);
    let p1 = BlockPos::new(0, 0, 0);
    let p2 = BlockPos::new(127, 63, 127);
    let pattern = WorldEditPattern::from_str("stone").unwrap();
    let dirt = WorldEditPattern::from_str("dirt").unwrap();

    let mixed = WorldEditPattern::from_str("stone,dirt,oak_planks,glass,gravel,sand,cobblestone").unwrap();
    time("old //set mixed (empty)", || {
        let _undo = create_clipboard(&mut world, p1, p1, p2);
        for x in 0..=127 {
            for y in 0..=63 {
                for z in 0..=127 {
                    world.set_block_raw(BlockPos::new(x, y, z), mixed.pick().get_id());
                }
            }
        }
        world.flush_block_changes();
    });
    time("old //set stone (over mixed)", || {
        let _undo = create_clipboard(&mut world, p1, p1, p2);
        for x in 0..=127 {
            for y in 0..=63 {
                for z in 0..=127 {
                    world.set_block_raw(BlockPos::new(x, y, z), pattern.pick().get_id());
                }
            }
        }
        world.flush_block_changes();
    });
    time("old //replace", || {
        let _undo = create_clipboard(&mut world, p1, p1, p2);
        for x in 0..=127 {
            for y in 0..=63 {
                for z in 0..=127 {
                    let pos = BlockPos::new(x, y, z);
                    if pattern.matches(world.get_block(pos)) {
                        world.set_block_raw(pos, dirt.pick().get_id());
                    }
                }
            }
        }
        world.flush_block_changes();
    });
    let cb = time("old //copy", || create_clipboard(&mut world, p1, p1, p2));
    time("old //paste", || {
        let target = BlockPos::new(200, 0, 200);
        let _undo = create_clipboard(&mut world, target, target, target + (p2 - p1));
        paste_clipboard(&mut world, &cb, target, false);
        world.flush_block_changes();
    });
    time("old //undo (paste + redo cb)", || {
        let target = BlockPos::new(200, 0, 200);
        let _redo = create_clipboard(&mut world, target, target, target + (p2 - p1));
        paste_clipboard(&mut world, &cb, target, false);
        world.flush_block_changes();
    });
}
