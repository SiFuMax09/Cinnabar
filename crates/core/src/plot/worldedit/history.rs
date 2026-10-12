//! Memory efficient //undo and //redo history.
//!
//! Instead of copying the whole edited area, every changed block is recorded as the state it had
//! before the edit. Consecutive blocks of a section that had the same state are stored as a
//! single run, so filling an area with a pattern costs a few bytes per section instead of
//! several bytes per block. Undoing an entry records the inverse entry, which becomes the redo
//! entry, so no new states are stored for redo.

use mchprs_blocks::block_entities::BlockEntity;
use mchprs_blocks::BlockPos;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use std::mem;

/// The maximum number of undo steps kept per player
const MAX_ENTRIES: usize = 100;
/// The maximum (approximate) memory in bytes the history of a player may use
const MAX_BYTES: usize = 256 * 1024 * 1024;

/// `len` consecutive blocks of a section, starting at section index `start`, that were `old` before
/// the edit. The index of a block in a section is `y << 8 | z << 4 | x`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub start: u16,
    pub len: u16,
    pub old: u16,
}

#[derive(Clone, Debug)]
pub struct SectionChanges {
    pub chunk_x: i32,
    pub chunk_z: i32,
    pub section_y: u16,
    /// In the order they were recorded
    pub runs: Vec<Run>,
}

/// Adds a changed block to a list of runs, extending the last run when it fits
pub fn push_change(runs: &mut Vec<Run>, idx: u16, old: u16) {
    if let Some(last) = runs.last_mut()
        && last.old == old
        && last.len < u16::MAX
        && last.start as u32 + last.len as u32 == idx as u32
    {
        last.len += 1;
        return;
    }
    runs.push(Run {
        start: idx,
        len: 1,
        old,
    });
}

/// Everything that changed in one edit
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub plot_x: i32,
    pub plot_z: i32,
    pub sections: Vec<SectionChanges>,
    /// The block entity a position had before it was changed, in the order of the changes
    pub block_entities: Vec<(BlockPos, Option<BlockEntity>)>,
    /// How many blocks were changed
    pub blocks: usize,
}

impl HistoryEntry {
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty() && self.block_entities.is_empty()
    }

    /// An estimate of the memory the entry uses
    pub fn approx_bytes(&self) -> usize {
        let runs: usize = self
            .sections
            .iter()
            .map(|s| mem::size_of::<SectionChanges>() + s.runs.capacity() * mem::size_of::<Run>())
            .sum();
        runs + self.block_entities.len() * 128 + mem::size_of::<HistoryEntry>()
    }
}

/// Collects the changes of an edit
#[derive(Default)]
pub struct Recorder {
    sections: Vec<SectionChanges>,
    lookup: FxHashMap<(i32, i32, u16), usize>,
    /// The section that was used last, edits usually stay in a section for a while
    last: Option<((i32, i32, u16), usize)>,
    pub block_entities: Vec<(BlockPos, Option<BlockEntity>)>,
    pub blocks: usize,
}

impl Recorder {
    pub fn new() -> Recorder {
        Recorder::default()
    }

    /// The runs of a section, which is added to the recorder if it is not in it yet
    pub fn section_runs(&mut self, chunk_x: i32, chunk_z: i32, section_y: u16) -> &mut Vec<Run> {
        let key = (chunk_x, chunk_z, section_y);
        let idx = match self.last {
            Some((last_key, idx)) if last_key == key => idx,
            _ => {
                let sections = &mut self.sections;
                let idx = *self.lookup.entry(key).or_insert_with(|| {
                    sections.push(SectionChanges {
                        chunk_x,
                        chunk_z,
                        section_y,
                        runs: Vec::new(),
                    });
                    sections.len() - 1
                });
                self.last = Some((key, idx));
                idx
            }
        };
        &mut self.sections[idx].runs
    }

    pub fn record(&mut self, chunk_x: i32, chunk_z: i32, section_y: u16, idx: u16, old: u16) {
        push_change(self.section_runs(chunk_x, chunk_z, section_y), idx, old);
        self.blocks += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty() && self.block_entities.is_empty()
    }

    /// Turns the recorded changes into an entry and starts over
    pub fn take_entry(&mut self, plot_x: i32, plot_z: i32) -> HistoryEntry {
        let mut sections = mem::take(&mut self.sections);
        for section in &mut sections {
            section.runs.shrink_to_fit();
        }
        self.lookup.clear();
        self.last = None;
        HistoryEntry {
            plot_x,
            plot_z,
            sections,
            block_entities: mem::take(&mut self.block_entities),
            blocks: mem::take(&mut self.blocks),
        }
    }
}

/// The undo and redo stacks of a player
#[derive(Default)]
pub struct History {
    undo: VecDeque<HistoryEntry>,
    redo: VecDeque<HistoryEntry>,
    bytes: usize,
}

impl History {
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Memory in bytes the history approximately uses
    pub fn approx_bytes(&self) -> usize {
        self.bytes
    }

    /// Adds an entry to the undo stack. A new edit clears the redo stack, but the entry that
    /// is the result of a redo does not.
    pub fn push_undo(&mut self, entry: HistoryEntry, clear_redo: bool) {
        if entry.is_empty() {
            return;
        }
        if clear_redo {
            self.clear_redo();
        }
        self.bytes += entry.approx_bytes();
        self.undo.push_back(entry);
        self.trim();
    }

    pub fn push_redo(&mut self, entry: HistoryEntry) {
        if entry.is_empty() {
            return;
        }
        self.bytes += entry.approx_bytes();
        self.redo.push_back(entry);
        self.trim();
    }

    pub fn pop_undo(&mut self) -> Option<HistoryEntry> {
        let entry = self.undo.pop_back()?;
        self.bytes -= entry.approx_bytes();
        Some(entry)
    }

    pub fn pop_redo(&mut self) -> Option<HistoryEntry> {
        let entry = self.redo.pop_back()?;
        self.bytes -= entry.approx_bytes();
        Some(entry)
    }

    pub fn clear_redo(&mut self) {
        for entry in self.redo.drain(..) {
            self.bytes -= entry.approx_bytes();
        }
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
    }

    /// Forgets the oldest entries if the history is too big
    fn trim(&mut self) {
        while self.undo.len() > MAX_ENTRIES || (self.bytes > MAX_BYTES && self.undo.len() > 1) {
            let Some(entry) = self.undo.pop_front() else {
                break;
            };
            self.bytes -= entry.approx_bytes();
        }
        while self.redo.len() > MAX_ENTRIES || (self.bytes > MAX_BYTES && !self.redo.is_empty()) {
            let Some(entry) = self.redo.pop_front() else {
                break;
            };
            self.bytes -= entry.approx_bytes();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_with_blocks(n: u16) -> HistoryEntry {
        let mut recorder = Recorder::new();
        for i in 0..n {
            // Alternating old states, so there are no runs
            recorder.record(0, 0, 0, i, 1 + i % 2);
        }
        recorder.take_entry(0, 0)
    }

    #[test]
    fn consecutive_blocks_become_runs() {
        let mut runs = Vec::new();
        for idx in 0..100 {
            push_change(&mut runs, idx, 7);
        }
        assert_eq!(
            runs,
            vec![Run {
                start: 0,
                len: 100,
                old: 7
            }]
        );
    }

    #[test]
    fn different_state_or_gap_starts_a_new_run() {
        let mut runs = Vec::new();
        push_change(&mut runs, 0, 1);
        push_change(&mut runs, 1, 1);
        push_change(&mut runs, 2, 2);
        push_change(&mut runs, 4, 2);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].len, 2);
        assert_eq!(runs[1], Run { start: 2, len: 1, old: 2 });
        assert_eq!(runs[2], Run { start: 4, len: 1, old: 2 });
    }

    #[test]
    fn the_same_block_twice_keeps_both_changes_in_order() {
        let mut runs = Vec::new();
        push_change(&mut runs, 5, 1);
        push_change(&mut runs, 5, 2);
        assert_eq!(runs.len(), 2);
        // Undoing walks backwards, so the first old state wins
        assert_eq!(runs[0].old, 1);
    }

    #[test]
    fn recorder_groups_by_section() {
        let mut recorder = Recorder::new();
        recorder.record(0, 0, 0, 0, 1);
        recorder.record(1, 0, 0, 0, 1);
        recorder.record(0, 0, 0, 1, 1);
        recorder.record(0, 0, 3, 0, 1);
        let entry = recorder.take_entry(2, 3);
        assert_eq!(entry.sections.len(), 3);
        assert_eq!(entry.blocks, 4);
        assert_eq!((entry.plot_x, entry.plot_z), (2, 3));
        // The two blocks in the first section are a run
        assert_eq!(entry.sections[0].runs.len(), 1);
        assert_eq!(entry.sections[0].runs[0].len, 2);
        assert!(recorder.is_empty());
    }

    #[test]
    fn a_filled_area_is_tiny() {
        let mut recorder = Recorder::new();
        for idx in 0..4096u16 {
            recorder.record(0, 0, 0, idx, 0);
        }
        let entry = recorder.take_entry(0, 0);
        assert_eq!(entry.sections[0].runs.len(), 1);
        assert!(entry.approx_bytes() < 200);
    }

    #[test]
    fn undo_and_redo_stacks() {
        let mut history = History::default();
        history.push_undo(entry_with_blocks(3), true);
        history.push_undo(entry_with_blocks(4), true);
        assert_eq!(history.undo_len(), 2);
        let undone = history.pop_undo().unwrap();
        assert_eq!(undone.blocks, 4);
        history.push_redo(undone);
        assert_eq!(history.redo_len(), 1);
        // A redo does not clear the redo stack, a new edit does
        history.push_undo(entry_with_blocks(2), false);
        assert_eq!(history.redo_len(), 1);
        history.push_undo(entry_with_blocks(2), true);
        assert_eq!(history.redo_len(), 0);
    }

    #[test]
    fn empty_entries_are_ignored() {
        let mut history = History::default();
        history.push_undo(Recorder::new().take_entry(0, 0), true);
        assert_eq!(history.undo_len(), 0);
        assert!(history.pop_undo().is_none());
    }

    #[test]
    fn history_is_limited() {
        let mut history = History::default();
        for _ in 0..MAX_ENTRIES + 20 {
            history.push_undo(entry_with_blocks(2), true);
        }
        assert_eq!(history.undo_len(), MAX_ENTRIES);
    }

    #[test]
    fn byte_accounting_returns_to_zero() {
        let mut history = History::default();
        history.push_undo(entry_with_blocks(10), true);
        history.push_undo(entry_with_blocks(10), true);
        assert!(history.approx_bytes() > 0);
        let a = history.pop_undo().unwrap();
        history.push_redo(a);
        history.clear_redo();
        history.pop_undo().unwrap();
        assert_eq!(history.approx_bytes(), 0);
    }
}
