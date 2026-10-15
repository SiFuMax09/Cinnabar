//! Time sliced edits.
//!
//! Big operations are split into small work items (a part of a chunk section each). The plot runs
//! as many items as fit in a time budget every update and sends the changes after every slice,
//! so a huge //set never stalls the plot tick for long. Jobs run one after another in the order
//! the commands were issued.

use super::history::{HistoryEntry, Recorder};
use super::pattern::{global_transform, GlobalTransform, Mask};
use super::session::{clamp_to_plot, EditSession};
use crate::player::{PacketSender, Player};
use crate::plot::{Plot, PlotWorld};
use cinnabar_blocks::BlockPos;
use cinnabar_world::World;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long the edits of one plot update may take
pub const SLICE_BUDGET: Duration = Duration::from_millis(12);
/// How often a progress message is sent for slow jobs
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

pub enum WorkItem {
    Box(BlockPos, BlockPos),
    /// A box that belongs to the copy with this number
    Copy(usize, BlockPos, BlockPos),
    Index(usize),
}

pub type RunFn = Box<dyn FnMut(&mut EditSession<'_>, &WorkItem) -> usize>;

pub struct StageBody {
    pub items: Vec<WorkItem>,
    pub run: RunFn,
}

pub struct StageEnv<'a> {
    pub world: &'a PlotWorld,
    pub player: Option<&'a mut Player>,
}

type Prepare = Box<dyn FnOnce(&mut StageEnv<'_>) -> Result<StageBody, String>>;

/// Where the history entry of a stage goes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryDest {
    /// A new edit, forgets what could be redone
    Undo,
    /// The result of //redo
    UndoKeepRedo,
    /// The result of //undo
    Redo,
}

pub struct Stage {
    prepare: Option<Prepare>,
    body: Option<StageBody>,
    next: usize,
    commit: Option<HistoryDest>,
}

impl Stage {
    pub fn new(items: Vec<WorkItem>, run: RunFn) -> Stage {
        Stage {
            prepare: None,
            body: Some(StageBody { items, run }),
            next: 0,
            commit: None,
        }
    }

    /// A stage whose work is decided when it starts, from the world as it is by then
    pub fn lazy(
        prepare: impl FnOnce(&mut StageEnv<'_>) -> Result<StageBody, String> + 'static,
    ) -> Stage {
        Stage {
            prepare: Some(Box::new(prepare)),
            body: None,
            next: 0,
            commit: None,
        }
    }

    /// Stores the changes of the job so far in the history of the player when the stage is done
    pub fn commit_to(mut self, dest: HistoryDest) -> Stage {
        self.commit = Some(dest);
        self
    }
}

pub struct JobStats {
    pub blocks: usize,
    pub elapsed: Duration,
}

type FinishFn = Box<dyn FnOnce(&mut Player, &JobStats)>;

pub struct EditJob {
    owner: u128,
    stages: VecDeque<Stage>,
    total_stages: usize,
    done_stages: usize,
    recorder: Recorder,
    mask: Option<Mask>,
    transform: Option<GlobalTransform>,
    blocks: usize,
    started: Instant,
    last_progress: Instant,
    finish: Option<FinishFn>,
    error: Option<String>,
    /// Whether no message is sent when the job is done
    quiet: bool,
    #[cfg(test)]
    last_entry: Option<HistoryEntry>,
}

impl EditJob {
    pub fn new(owner: u128, mask: Option<Mask>) -> EditJob {
        EditJob {
            owner,
            stages: VecDeque::new(),
            total_stages: 0,
            done_stages: 0,
            recorder: Recorder::new(),
            mask,
            transform: global_transform(owner),
            blocks: 0,
            started: Instant::now(),
            last_progress: Instant::now(),
            finish: None,
            error: None,
            quiet: false,
            #[cfg(test)]
            last_entry: None,
        }
    }

    /// Runs the job to the end without a player and returns the history entry of the last stage
    #[cfg(test)]
    pub fn run_for_test(&mut self, world: &mut PlotWorld) -> HistoryEntry {
        let far = Instant::now() + Duration::from_secs(3600);
        assert!(self.step(world, None, far));
        world.flush_block_changes();
        self.last_entry
            .take()
            .unwrap_or_else(|| Recorder::new().take_entry(0, 0))
    }

    #[cfg(test)]
    pub fn step_for_bench(&mut self, world: &mut PlotWorld, budget: Duration) -> bool {
        self.step(world, None, Instant::now() + budget)
    }

    pub fn stage(mut self, stage: Stage) -> EditJob {
        self.stages.push_back(stage);
        self.total_stages += 1;
        self
    }

    /// A job with a single stage that is stored in the undo history
    /// Replaces the transform the edit goes through (by default the global transform of the owner)
    pub fn with_transform(mut self, transform: Option<GlobalTransform>) -> EditJob {
        if transform.is_some() {
            self.transform = transform;
        }
        self
    }

    pub fn single(owner: u128, mask: Option<Mask>, items: Vec<WorkItem>, run: RunFn) -> EditJob {
        EditJob::new(owner, mask).stage(Stage::new(items, run).commit_to(HistoryDest::Undo))
    }

    /// Replaces the default message that is sent when the job is done
    pub fn on_finish(mut self, f: impl FnOnce(&mut Player, &JobStats) + 'static) -> EditJob {
        self.finish = Some(Box::new(f));
        self
    }

    pub fn quiet(mut self) -> EditJob {
        self.quiet = true;
        self
    }

    fn progress(&self) -> f32 {
        if self.total_stages == 0 {
            return 1.0;
        }
        let current = self.stages.front().map_or(0.0, |stage| match &stage.body {
            Some(body) if !body.items.is_empty() => stage.next as f32 / body.items.len() as f32,
            _ => 0.0,
        });
        (self.done_stages as f32 + current) / self.total_stages as f32
    }

    /// Runs work items until the deadline. Returns true when the job is done.
    fn step(
        &mut self,
        world: &mut PlotWorld,
        mut player: Option<&mut Player>,
        deadline: Instant,
    ) -> bool {
        let (plot_x, plot_z) = (world.x, world.z);
        while let Some(stage) = self.stages.front_mut() {
            if stage.body.is_none() {
                let prepare = stage.prepare.take().expect("a stage has a body or prepare");
                let mut env = StageEnv {
                    world,
                    player: player.as_deref_mut(),
                };
                match prepare(&mut env) {
                    Ok(body) => stage.body = Some(body),
                    Err(message) => {
                        self.error = Some(message);
                        self.stages.clear();
                        return true;
                    }
                }
            }
            let body = stage.body.as_mut().unwrap();
            if stage.next < body.items.len() {
                let mut session = EditSession::new(world, &mut self.recorder, self.mask.as_ref())
                    .with_transform(self.transform.as_ref());
                while stage.next < body.items.len() {
                    self.blocks += (body.run)(&mut session, &body.items[stage.next]);
                    stage.next += 1;
                    if stage.next < body.items.len() && Instant::now() >= deadline {
                        return false;
                    }
                }
            }
            let stage = self.stages.pop_front().unwrap();
            self.done_stages += 1;
            if let Some(dest) = stage.commit {
                let entry = self.recorder.take_entry(plot_x, plot_z);
                if let Some(player) = player.as_deref_mut() {
                    store_entry(player, entry, dest);
                } else {
                    #[cfg(test)]
                    {
                        self.last_entry = Some(entry);
                    }
                }
            }
            if Instant::now() >= deadline && !self.stages.is_empty() {
                return false;
            }
        }
        true
    }

    fn report(&mut self, player: Option<&mut Player>) {
        let Some(player) = player else { return };
        if let Some(error) = self.error.take() {
            player.send_error_message(&error);
            return;
        }
        let stats = JobStats {
            blocks: self.blocks,
            elapsed: self.started.elapsed(),
        };
        if let Some(finish) = self.finish.take() {
            finish(player, &stats);
        } else if !self.quiet {
            player.send_worldedit_message(&format!(
                "Operation completed: {} block(s) affected ({:?})",
                stats.blocks, stats.elapsed
            ));
        }
    }
}

fn store_entry(player: &mut Player, entry: HistoryEntry, dest: HistoryDest) {
    match dest {
        HistoryDest::Undo => player.worldedit_history.push_undo(entry, true),
        HistoryDest::UndoKeepRedo => player.worldedit_history.push_undo(entry, false),
        HistoryDest::Redo => player.worldedit_history.push_redo(entry),
    }
}

/// Splits a box into pieces that are inside of a single chunk section each
pub fn section_items(world: &PlotWorld, min: BlockPos, max: BlockPos) -> Vec<WorkItem> {
    let Some((min, max)) = clamp_to_plot(world, min, max) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for chunk_x in (min.x >> 4)..=(max.x >> 4) {
        for chunk_z in (min.z >> 4)..=(max.z >> 4) {
            for section_y in (min.y >> 4)..=(max.y >> 4) {
                items.push(WorkItem::Box(
                    BlockPos::new(
                        min.x.max(chunk_x << 4),
                        min.y.max(section_y << 4),
                        min.z.max(chunk_z << 4),
                    ),
                    BlockPos::new(
                        max.x.min((chunk_x << 4) + 15),
                        max.y.min((section_y << 4) + 15),
                        max.z.min((chunk_z << 4) + 15),
                    ),
                ));
            }
        }
    }
    items
}

/// Like [`section_items`] for several boxes
pub fn section_items_of(world: &PlotWorld, boxes: &[(BlockPos, BlockPos)]) -> Vec<WorkItem> {
    boxes
        .iter()
        .flat_map(|(min, max)| section_items(world, *min, *max))
        .collect()
}

/// One item per 16x16 column tile of the box (with its full height), for operations that look at
/// whole columns
pub fn column_items(world: &PlotWorld, min: BlockPos, max: BlockPos) -> Vec<WorkItem> {
    let Some((min, max)) = clamp_to_plot(world, min, max) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for chunk_x in (min.x >> 4)..=(max.x >> 4) {
        for chunk_z in (min.z >> 4)..=(max.z >> 4) {
            items.push(WorkItem::Box(
                BlockPos::new(min.x.max(chunk_x << 4), min.y, min.z.max(chunk_z << 4)),
                BlockPos::new(
                    max.x.min((chunk_x << 4) + 15),
                    max.y,
                    max.z.min((chunk_z << 4) + 15),
                ),
            ));
        }
    }
    items
}

/// Items `Index(0)..Index(count.div_ceil(per_item))`
pub fn index_items(count: usize, per_item: usize) -> Vec<WorkItem> {
    (0..count.div_ceil(per_item)).map(WorkItem::Index).collect()
}

pub const POSITIONS_PER_ITEM: usize = 4096;

impl Plot {
    pub(super) fn queue_edit_job(&mut self, job: EditJob) {
        self.edit_jobs.push_back(job);
    }

    /// Runs queued edits for up to `budget` and sends the changes
    pub(crate) fn run_edit_jobs(&mut self, budget: Duration) {
        if self.edit_jobs.is_empty() {
            return;
        }
        // The blocks are edited behind the back of redpiler
        self.reset_redpiler();
        let started = Instant::now();
        let deadline = started + budget;
        while let Some(job) = self.edit_jobs.front_mut() {
            let player_idx = self.players.iter().position(|p| p.uuid == job.owner);
            let player = player_idx.map(|idx| &mut self.players[idx]);
            let finished = job.step(&mut self.world, player, deadline);
            let player = player_idx.map(|idx| &mut self.players[idx]);
            if finished {
                let mut job = self.edit_jobs.pop_front().unwrap();
                // Block entities are sent with the blocks, so publish before reporting
                self.world.flush_block_changes();
                let player = player_idx.map(|idx| &mut self.players[idx]);
                job.report(player);
            } else {
                if job.last_progress.elapsed() >= PROGRESS_INTERVAL {
                    job.last_progress = Instant::now();
                    if let Some(player) = player {
                        player.send_worldedit_progress(job.progress());
                    }
                }
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        self.publish_world();
        // This took a while on purpose, it is not a lagging plot
        self.reset_timings();
    }

    /// Runs all queued edits right now. Used when something needs the finished world.
    pub(crate) fn finish_edit_jobs(&mut self) {
        while !self.edit_jobs.is_empty() {
            self.run_edit_jobs(Duration::from_secs(3600));
        }
    }
}

impl Plot {
    /// Stops the queued edits of the player (`//cancel`). What was already changed by a running
    /// edit stays and can be undone. Returns how many edits were cancelled.
    pub(super) fn cancel_edit_jobs(&mut self, owner: u128) -> usize {
        let (plot_x, plot_z) = (self.world.x, self.world.z);
        let mut cancelled = 0;
        let mut kept = VecDeque::new();
        while let Some(mut job) = self.edit_jobs.pop_front() {
            if job.owner != owner {
                kept.push_back(job);
                continue;
            }
            cancelled += 1;
            if !job.recorder.is_empty() {
                let entry = job.recorder.take_entry(plot_x, plot_z);
                if let Some(player) = self.players.iter_mut().find(|p| p.uuid == owner) {
                    store_entry(player, entry, HistoryDest::Undo);
                }
            }
        }
        self.edit_jobs = kept;
        cancelled
    }
}
