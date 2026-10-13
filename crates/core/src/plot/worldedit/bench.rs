//! Benchmarks for the bulk operations. Run with
//! `cargo test --release -p cinnabar_core worldedit::bench -- --ignored --nocapture`
//!
//! The numbers of the old implementation (per-block pattern picking, a full copy of the area for
//! undo, per-block clipboard access) for the same operations on a 128x64x128 area (1M blocks)
//! on the same machine were:
//!
//! ```text
//! //set mixed (empty)      170 ms     //copy                  68 ms
//! //set stone (over mixed) 168 ms     //paste                121 ms
//! //replace                185 ms     //undo                  48 ms
//! ```

use super::history::HistoryEntry;
use super::job::*;
use super::ops;
use super::pattern::{Mask, Pattern};
use super::session::{apply_section_changes, create_clipboard, EditSession};
use super::*;
use std::time::{Duration, Instant};

fn time<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    println!("{:<28} {:?}", name, start.elapsed());
    result
}

fn run(world: &mut PlotWorld, mut job: EditJob) -> HistoryEntry {
    job.run_for_test(world)
}

/// The slice that took longest when a job is run the way the plot does it
fn longest_slice(world: &mut PlotWorld, mut job: EditJob) -> (usize, Duration) {
    let mut slices = 0;
    let mut longest = Duration::ZERO;
    loop {
        let start = Instant::now();
        let done = job.step_for_bench(world, SLICE_BUDGET);
        world.flush_block_changes();
        longest = longest.max(start.elapsed());
        slices += 1;
        if done {
            return (slices, longest);
        }
    }
}

#[test]
#[ignore]
fn new_pipeline() {
    let mut world = PlotWorld::empty_for_test(0, 0);
    let p1 = BlockPos::new(0, 0, 0);
    let p2 = BlockPos::new(127, 63, 127);
    let pattern = |s: &str| Pattern::parse(s).unwrap();

    let job = ops::region_job(
        &world,
        0,
        None,
        None,
        p1,
        p2,
        pattern("stone,dirt,oak_planks,glass,gravel,sand,cobblestone"),
        None,
        None,
    );
    time("//set mixed (empty)", || run(&mut world, job));
    // Where the time goes: editing the chunk sections or sending the changes
    let mut job = ops::region_job(&world, 0, None, None, p1, p2, pattern("glass"), None, None);
    time("  edit only", || {
        job.step_for_bench(&mut world, Duration::from_secs(60))
    });
    time("  flush only", || world.flush_block_changes());
    let job = ops::region_job(&world, 0, None, None, p1, p2, pattern("stone"), None, None);
    let entry = time("//set stone (over mixed)", || run(&mut world, job));
    println!(
        "  undo entry: ~{} bytes for {} blocks",
        entry.approx_bytes(),
        entry.blocks
    );
    let job = ops::region_job(
        &world,
        0,
        None,
        Some(Mask::parse("stone").unwrap()),
        p1,
        p2,
        pattern("dirt"),
        None,
        None,
    );
    time("//replace", || run(&mut world, job));
    let cb = time("//copy", || create_clipboard(&world, p1, p1, p2));
    let target = BlockPos::new(200, 0, 200);
    let job = ops::paste_job(&world, 0, None, cb, vec![target], false);
    let entry = time("//paste", || run(&mut world, job));
    let mut redo = history::Recorder::new();
    time("//undo", || {
        let mut session = EditSession::new(&mut world, &mut redo, None);
        for i in 0..entry.sections.len() {
            apply_section_changes(&mut session, &entry, i);
        }
        world.flush_block_changes();
    });

    // How the plot slices a big edit
    let job = ops::region_job(
        &world,
        0,
        None,
        None,
        BlockPos::new(0, 0, 0),
        BlockPos::new(511, 127, 511),
        pattern("stone,dirt"),
        None,
        None,
    );
    let start = Instant::now();
    let (slices, longest) = longest_slice(&mut world, job);
    println!(
        "//set 512x128x512 (33M blocks) in {:?}: {} slices, longest {:?}",
        start.elapsed(),
        slices,
        longest
    );
}
