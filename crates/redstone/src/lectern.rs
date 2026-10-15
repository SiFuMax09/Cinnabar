//! The block side of lecterns: holding a book, the page it is opened at and the redstone signals
//! that come with it. Opening the screen and the page buttons are up to the plot.

use cinnabar_blocks::block_entities::{BlockEntity, InventoryEntry};
use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::{generic, BlockFace, BlockPos};
use cinnabar_world::{TickPriority, World};

/// The state of a lectern is `has_book` and `powered`. A page change powers it for one redstone
/// tick.
const PULSE_TICKS: u32 = 1;

pub fn is_lectern(id: u32) -> bool {
    generic::state_name(id) == "minecraft:lectern"
}

fn flag(id: u32, name: &str) -> bool {
    generic::get_property(id, name) == Some("true")
}

pub fn has_book(id: u32) -> bool {
    flag(id, "has_book")
}

pub fn is_powered(id: u32) -> bool {
    flag(id, "powered")
}

/// Whether the item can be put on a lectern
pub fn is_lectern_book(item_name: &str) -> bool {
    matches!(
        item_name,
        "minecraft:written_book" | "minecraft:writable_book"
    )
}

/// A powered lectern powers all blocks around it weakly
pub fn weak_power(id: u32) -> u8 {
    if is_powered(id) {
        15
    } else {
        0
    }
}

/// A powered lectern powers the block below it strongly. `side` is the direction from the block
/// that is asked to the lectern.
pub fn strong_power(id: u32, side: BlockFace) -> u8 {
    if side == BlockFace::Top {
        weak_power(id)
    } else {
        0
    }
}

/// The signal strength a comparator reads from a lectern, which depends on how far the book is
/// read
pub fn analog_output(world: &impl World, pos: BlockPos, id: u32) -> u8 {
    if !has_book(id) {
        return 0;
    }
    match world.get_block_entity(pos) {
        Some(BlockEntity::Lectern {
            page, page_count, ..
        }) => {
            let progress = if *page_count > 1 {
                *page as f32 / (*page_count - 1) as f32
            } else {
                1.0
            };
            (progress * 14.0).floor() as u8 + 1
        }
        _ => 1,
    }
}

/// The page the lectern is opened at and the number of pages of its book
pub fn page_info(world: &impl World, pos: BlockPos) -> Option<(i32, i32)> {
    match world.get_block_entity(pos) {
        Some(BlockEntity::Lectern {
            book: Some(_),
            page,
            page_count,
        }) => Some((*page, *page_count)),
        _ => None,
    }
}

/// The book on the lectern
pub fn book(world: &impl World, pos: BlockPos) -> Option<InventoryEntry> {
    match world.get_block_entity(pos) {
        Some(BlockEntity::Lectern { book, .. }) => book.clone(),
        _ => None,
    }
}

fn lectern_id(world: &impl World, pos: BlockPos) -> Option<u32> {
    match world.get_block(pos) {
        Block::Generic(id) if is_lectern(id) => Some(id),
        _ => None,
    }
}

/// Updates everything that reads the lectern: the blocks around it, comparators behind them and
/// the block below, which it powers strongly
fn update_neighbors(world: &mut impl World, pos: BlockPos) {
    super::update_surrounding_blocks(world, pos);
    super::update_surrounding_blocks(world, pos.offset(BlockFace::Bottom));
    super::transport::update_comparators_around(world, pos);
}

fn set_flags(world: &mut impl World, pos: BlockPos, id: u32, has_book: bool, powered: bool) {
    let id = generic::with_property(id, "has_book", if has_book { "true" } else { "false" });
    let id = generic::with_property(id, "powered", if powered { "true" } else { "false" });
    world.set_block(pos, Block::Generic(id));
}

/// Puts the book on an empty lectern, opened at the first page. Returns false if the block is
/// no lectern or already has a book.
pub fn place_book(world: &mut impl World, pos: BlockPos, book: InventoryEntry) -> bool {
    let Some(id) = lectern_id(world, pos) else {
        return false;
    };
    if has_book(id) {
        return false;
    }
    world.set_block_entity(pos, BlockEntity::lectern(Some(book)));
    set_flags(world, pos, id, true, is_powered(id));
    update_neighbors(world, pos);
    true
}

/// Takes the book off the lectern
pub fn take_book(world: &mut impl World, pos: BlockPos) -> Option<InventoryEntry> {
    let id = lectern_id(world, pos)?;
    let book = book(world, pos)?;
    world.delete_block_entity(pos);
    set_flags(world, pos, id, false, false);
    update_neighbors(world, pos);
    Some(book)
}

/// Turns the page of the lectern to the page, which is limited to the pages the book has.
/// Returns whether the page changed.
pub fn set_page(world: &mut impl World, pos: BlockPos, page: i32) -> bool {
    let Some(id) = lectern_id(world, pos) else {
        return false;
    };
    let Some(BlockEntity::Lectern {
        book,
        page: old_page,
        page_count,
    }) = world.get_block_entity(pos).cloned()
    else {
        return false;
    };
    let page = page.clamp(0, (page_count - 1).max(0));
    if book.is_none() || page == old_page {
        return false;
    }
    world.set_block_entity(
        pos,
        BlockEntity::Lectern {
            book,
            page,
            page_count,
        },
    );
    // The page change is a short redstone pulse
    set_flags(world, pos, id, true, true);
    world.schedule_tick(pos, PULSE_TICKS, TickPriority::Normal);
    update_neighbors(world, pos);
    true
}

/// Ends the pulse of a page change
pub fn tick(world: &mut impl World, pos: BlockPos, id: u32) {
    if !is_lectern(id) || !is_powered(id) {
        return;
    }
    set_flags(world, pos, id, has_book(id), false);
    update_neighbors(world, pos);
}

/// Updates the blocks around a lectern that stops existing while it is outputting power
pub fn on_removed(world: &mut impl World, pos: BlockPos) {
    super::update_surrounding_blocks(world, pos.offset(BlockFace::Bottom));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comparator;
    use cinnabar_world::testing::TestWorld;

    fn lectern(world: &mut TestWorld, pos: BlockPos) {
        let id = generic::default_state("minecraft:lectern").unwrap();
        let id = generic::with_property(id, "has_book", "false");
        let id = generic::with_property(id, "powered", "false");
        world.set_block(pos, Block::Generic(id));
    }

    fn id_at(world: &TestWorld, pos: BlockPos) -> u32 {
        match world.get_block(pos) {
            Block::Generic(id) => id,
            other => panic!("no lectern but {other:?}"),
        }
    }

    fn book(_pages: i32) -> InventoryEntry {
        InventoryEntry {
            id: 0,
            slot: 0,
            count: 1,
            nbt: None,
        }
    }

    fn with_book(world: &mut TestWorld, pos: BlockPos, pages: i32) {
        lectern(world, pos);
        assert!(place_book(world, pos, book(pages)));
        world.set_block_entity(
            pos,
            BlockEntity::Lectern {
                book: Some(book(pages)),
                page: 0,
                page_count: pages,
            },
        );
    }

    #[test]
    fn placing_and_taking_the_book() {
        let mut world = TestWorld::new(1, 1, 1);
        let pos = BlockPos::new(2, 2, 2);
        lectern(&mut world, pos);
        assert!(!has_book(id_at(&world, pos)));
        assert!(place_book(&mut world, pos, book(3)));
        assert!(has_book(id_at(&world, pos)));
        assert!(!place_book(&mut world, pos, book(3)));

        assert!(take_book(&mut world, pos).is_some());
        assert!(!has_book(id_at(&world, pos)));
        assert!(world.get_block_entity(pos).is_none());
        assert!(take_book(&mut world, pos).is_none());
    }

    #[test]
    fn page_turns_are_limited_and_pulse() {
        let mut world = TestWorld::new(1, 1, 1);
        let pos = BlockPos::new(2, 2, 2);
        with_book(&mut world, pos, 3);
        assert!(!set_page(&mut world, pos, 0));
        assert!(!is_powered(id_at(&world, pos)));

        assert!(set_page(&mut world, pos, 1));
        assert_eq!(page_info(&world, pos), Some((1, 3)));
        let id = id_at(&world, pos);
        assert!(is_powered(id) && has_book(id));
        assert_eq!(weak_power(id), 15);
        assert_eq!(strong_power(id, BlockFace::Top), 15);
        assert_eq!(strong_power(id, BlockFace::Bottom), 0);

        // The last page is the limit
        assert!(set_page(&mut world, pos, 10));
        assert_eq!(page_info(&world, pos), Some((2, 3)));
        assert!(!set_page(&mut world, pos, 3));

        let current = id_at(&world, pos);
        tick(&mut world, pos, current);
        let id = id_at(&world, pos);
        assert!(!is_powered(id) && has_book(id));
    }

    #[test]
    fn comparator_reads_the_progress() {
        let mut world = TestWorld::new(1, 1, 1);
        let pos = BlockPos::new(2, 2, 2);
        lectern(&mut world, pos);
        assert_eq!(
            comparator::get_override(world.get_block(pos), &world, pos),
            0
        );

        with_book(&mut world, pos, 5);
        assert!(comparator::has_override(world.get_block(pos)));
        let read = |world: &TestWorld| comparator::get_override(world.get_block(pos), world, pos);
        assert_eq!(read(&world), 1);
        set_page(&mut world, pos, 2);
        assert_eq!(read(&world), 8);
        set_page(&mut world, pos, 4);
        assert_eq!(read(&world), 15);
    }
}
