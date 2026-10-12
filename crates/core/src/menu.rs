//! The state of an open container screen and the handling of the clicks in it.
//!
//! This is a port of `AbstractContainerMenu.doClick` and `moveItemStackTo` for the menus of
//! chest-like containers: the slots of the container come first, followed by the 27 slots of the
//! player's inventory and the 9 slots of the hotbar. The menu works on a copy of the slots, so
//! the caller is responsible for loading the container before a click and storing it after.

use mchprs_blocks::block_entities::{ContainerType, Inventory, InventoryEntry};
use mchprs_blocks::items::ItemStack;
use mchprs_blocks::BlockPos;

/// The slots of the main inventory in the menu come after the slots of the container
const MAIN_SLOTS: usize = 27;
const HOTBAR_SLOTS: usize = 9;
/// The index in the inventory of the player of the first main inventory slot
const MAIN_START: usize = 9;
const HOTBAR_START: usize = 36;
const OFFHAND: usize = 45;
/// The slot index the client sends when it clicks outside of the screen
pub const OUTSIDE_SLOT: i32 = -999;

/// How the slot was clicked, `ContainerInput` in the game
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickMode {
    Pickup,
    QuickMove,
    Swap,
    Clone,
    Throw,
    QuickCraft,
    PickupAll,
}

impl ClickMode {
    pub fn from_id(id: i32) -> ClickMode {
        match id {
            1 => ClickMode::QuickMove,
            2 => ClickMode::Swap,
            3 => ClickMode::Clone,
            4 => ClickMode::Throw,
            5 => ClickMode::QuickCraft,
            6 => ClickMode::PickupAll,
            _ => ClickMode::Pickup,
        }
    }
}

pub fn same_item(a: &ItemStack, b: &ItemStack) -> bool {
    a.item_type.get_id() == b.item_type.get_id()
        && a.nbt.as_ref().map(|nbt| &nbt.content) == b.nbt.as_ref().map(|nbt| &nbt.content)
}

fn max_stack(stack: &ItemStack) -> u8 {
    stack.item_type.max_stack_size().min(u8::MAX as u32) as u8
}

pub fn entry_to_stack(entry: &InventoryEntry) -> ItemStack {
    crate::utils::inventory_entry_to_stack(entry)
}

pub fn stack_to_entry(stack: &ItemStack, slot: usize) -> InventoryEntry {
    InventoryEntry {
        id: stack.item_type.get_id(),
        slot: slot as i8,
        count: stack.count as i8,
        nbt: stack.nbt.clone().map(|blob| {
            let mut data = Vec::new();
            blob.to_writer(&mut data).unwrap();
            data
        }),
    }
}

/// Removes up to `amount` items from the stack
fn split(stack: &mut ItemStack, amount: u8) -> ItemStack {
    let taken = amount.min(stack.count);
    stack.count -= taken;
    ItemStack {
        count: taken,
        ..stack.clone()
    }
}

#[derive(Debug)]
pub struct Menu {
    pub window_id: i32,
    pub state_id: i32,
    pub pos: BlockPos,
    pub ty: ContainerType,
    /// The container, the main inventory, the hotbar and the offhand slot of the player, which
    /// can be a target of swaps but is not shown
    slots: Vec<Option<ItemStack>>,
    pub carried: Option<ItemStack>,
    quick_craft_status: i32,
    quick_craft_type: i32,
    quick_craft_slots: Vec<usize>,
    infinite_materials: bool,
}

impl Menu {
    pub fn new(
        window_id: i32,
        pos: BlockPos,
        ty: ContainerType,
        container: &Inventory,
        player_inventory: &[Option<ItemStack>],
        infinite_materials: bool,
    ) -> Menu {
        let mut menu = Menu {
            window_id,
            state_id: 0,
            pos,
            ty,
            slots: vec![None; ty.num_slots() as usize + MAIN_SLOTS + HOTBAR_SLOTS + 1],
            carried: None,
            quick_craft_status: 0,
            quick_craft_type: 0,
            quick_craft_slots: Vec::new(),
            infinite_materials,
        };
        menu.load(container, player_inventory);
        menu
    }

    pub fn container_size(&self) -> usize {
        self.ty.num_slots() as usize
    }

    /// The number of slots the client knows about
    pub fn shown_slots(&self) -> usize {
        self.slots.len() - 1
    }

    pub fn slots(&self) -> &[Option<ItemStack>] {
        &self.slots[..self.shown_slots()]
    }

    /// Replaces the slots by the current items of the container and the player
    pub fn load(&mut self, container: &Inventory, player_inventory: &[Option<ItemStack>]) {
        let size = self.container_size();
        for slot in 0..size {
            self.slots[slot] = container.get(slot).map(entry_to_stack);
        }
        for slot in 0..MAIN_SLOTS {
            self.slots[size + slot] = player_inventory[MAIN_START + slot].clone();
        }
        for slot in 0..HOTBAR_SLOTS {
            self.slots[size + MAIN_SLOTS + slot] = player_inventory[HOTBAR_START + slot].clone();
        }
        self.slots[size + MAIN_SLOTS + HOTBAR_SLOTS] = player_inventory[OFFHAND].clone();
    }

    pub fn container_inventory(&self) -> Inventory {
        let size = self.container_size();
        let mut inventory = Inventory::new(size);
        for slot in 0..size {
            if let Some(stack) = self.slots[slot].as_ref().filter(|stack| stack.count > 0) {
                inventory.set(slot, Some(stack_to_entry(stack, slot)));
            }
        }
        inventory
    }

    /// Writes the slots of the player back. Returns the inventory slots that changed.
    pub fn store_player_inventory(&self, player_inventory: &mut [Option<ItemStack>]) -> Vec<usize> {
        let size = self.container_size();
        let mut changed = Vec::new();
        let mut store = |inventory_slot: usize, menu_slot: usize| {
            let new = self.slots[menu_slot]
                .clone()
                .filter(|stack| stack.count > 0);
            let old = &player_inventory[inventory_slot];
            let equal = match (old, &new) {
                (None, None) => true,
                (Some(a), Some(b)) => a.count == b.count && same_item(a, b),
                _ => false,
            };
            if !equal {
                player_inventory[inventory_slot] = new;
                changed.push(inventory_slot);
            }
        };
        for slot in 0..MAIN_SLOTS {
            store(MAIN_START + slot, size + slot);
        }
        for slot in 0..HOTBAR_SLOTS {
            store(HOTBAR_START + slot, size + MAIN_SLOTS + slot);
        }
        store(OFFHAND, size + MAIN_SLOTS + HOTBAR_SLOTS);
        changed
    }

    fn get(&self, slot: usize) -> Option<&ItemStack> {
        self.slots[slot].as_ref().filter(|stack| stack.count > 0)
    }

    fn set(&mut self, slot: usize, stack: Option<ItemStack>) {
        self.slots[slot] = stack.filter(|stack| stack.count > 0);
    }

    /// Whether the item is allowed in the slot, `Slot.mayPlace`
    fn may_place(&self, slot: usize, stack: &ItemStack) -> bool {
        if slot >= self.container_size() {
            return true;
        }
        match self.ty {
            // The result slot of a furnace cannot be filled
            ContainerType::Furnace => slot != 2,
            ContainerType::ShulkerBox => !stack.item_type.get_name().ends_with("shulker_box"),
            _ => true,
        }
    }

    /// `Slot.safeInsert`: puts up to `amount` of the stack into the slot, returns the rest
    fn safe_insert(&mut self, slot: usize, mut stack: ItemStack, amount: u8) -> Option<ItemStack> {
        if stack.count == 0 || !self.may_place(slot, &stack) {
            return Some(stack);
        }
        let current_count = self.get(slot).map_or(0, |current| current.count);
        let room = max_stack(&stack).saturating_sub(current_count);
        let insert = amount.min(stack.count).min(room);
        if insert == 0 {
            return Some(stack);
        }
        match self.get(slot).cloned() {
            None => {
                let placed = split(&mut stack, insert);
                self.set(slot, Some(placed));
            }
            Some(mut current) if same_item(&current, &stack) => {
                stack.count -= insert;
                current.count += insert;
                self.set(slot, Some(current));
            }
            Some(_) => {}
        }
        (stack.count > 0).then_some(stack)
    }

    /// `Slot.tryRemove`
    fn try_remove(&mut self, slot: usize, amount: u8, max: u8) -> Option<ItemStack> {
        let mut current = self.get(slot)?.clone();
        let taken = split(&mut current, amount.min(max));
        if taken.count == 0 {
            return None;
        }
        self.set(slot, Some(current));
        Some(taken)
    }

    /// `AbstractContainerMenu.moveItemStackTo`. Returns whether anything moved.
    fn move_item_stack_to(
        &mut self,
        stack: &mut ItemStack,
        start: usize,
        end: usize,
        backwards: bool,
    ) -> bool {
        let order: Vec<usize> = if backwards {
            (start..end).rev().collect()
        } else {
            (start..end).collect()
        };
        let mut changed = false;
        if max_stack(stack) > 1 {
            for &dest in &order {
                if stack.count == 0 {
                    break;
                }
                let Some(target) = self.get(dest).cloned() else {
                    continue;
                };
                if !same_item(&target, stack) {
                    continue;
                }
                let max = max_stack(&target);
                let total = target.count as u32 + stack.count as u32;
                if total <= max as u32 {
                    stack.count = 0;
                    self.set(
                        dest,
                        Some(ItemStack {
                            count: total as u8,
                            ..target
                        }),
                    );
                    changed = true;
                } else if target.count < max {
                    stack.count -= max - target.count;
                    self.set(
                        dest,
                        Some(ItemStack {
                            count: max,
                            ..target
                        }),
                    );
                    changed = true;
                }
            }
        }
        if stack.count > 0 {
            for &dest in &order {
                if self.get(dest).is_none() && self.may_place(dest, stack) {
                    let amount = stack.count.min(max_stack(stack));
                    let placed = split(stack, amount);
                    self.set(dest, Some(placed));
                    changed = true;
                    break;
                }
            }
        }
        changed
    }

    /// `quickMoveStack`: shift-click moves between the container and the inventory of the player
    fn quick_move_stack(&mut self, slot: usize) -> Option<ItemStack> {
        let mut stack = self.get(slot)?.clone();
        let original = stack.clone();
        let size = self.container_size();
        let total = self.shown_slots();
        let moved = if slot < size {
            self.move_item_stack_to(&mut stack, size, total, true)
        } else {
            self.move_item_stack_to(&mut stack, 0, size, false)
        };
        if !moved {
            return None;
        }
        self.set(slot, Some(stack));
        Some(original)
    }

    fn reset_quick_craft(&mut self) {
        self.quick_craft_status = 0;
        self.quick_craft_slots.clear();
    }

    fn can_quick_replace(&self, slot: usize, stack: &ItemStack) -> bool {
        match self.get(slot) {
            None => true,
            Some(current) => same_item(current, stack),
        }
    }

    /// Handles a click packet. Items that are thrown out of the menu are added to `dropped`.
    pub fn click(
        &mut self,
        slot_index: i32,
        button: i8,
        mode: ClickMode,
        dropped: &mut Vec<ItemStack>,
    ) {
        let shown = self.shown_slots() as i32;
        if slot_index != OUTSIDE_SLOT && (slot_index < -1 || slot_index >= shown) {
            return;
        }
        let slot = (slot_index >= 0).then_some(slot_index as usize);
        let button_i = button as i32;

        if mode == ClickMode::QuickCraft {
            self.quick_craft(slot, button_i, dropped);
            return;
        }
        if self.quick_craft_status != 0 {
            self.reset_quick_craft();
            return;
        }

        match mode {
            ClickMode::Pickup | ClickMode::QuickMove if button == 0 || button == 1 => {
                let primary = button == 0;
                if slot_index == OUTSIDE_SLOT {
                    if let Some(mut carried) = self.carried.take() {
                        if primary {
                            dropped.push(carried);
                        } else {
                            dropped.push(split(&mut carried, 1));
                            self.carried = (carried.count > 0).then_some(carried);
                        }
                    }
                } else if mode == ClickMode::QuickMove {
                    let Some(slot) = slot else { return };
                    let Some(mut clicked) = self.quick_move_stack(slot) else {
                        return;
                    };
                    // The game repeats until the stack in the slot is a different item
                    loop {
                        let same = self
                            .get(slot)
                            .is_some_and(|current| same_item(current, &clicked));
                        if !same {
                            break;
                        }
                        match self.quick_move_stack(slot) {
                            Some(next) => clicked = next,
                            None => break,
                        }
                    }
                } else if let Some(slot) = slot {
                    self.pickup(slot, primary);
                }
            }
            ClickMode::Swap if (0..9).contains(&button_i) || button_i == 40 => {
                let Some(slot) = slot else { return };
                self.swap(slot, button_i, dropped);
            }
            ClickMode::Clone if self.infinite_materials && self.carried.is_none() => {
                if let Some(slot) = slot
                    && let Some(stack) = self.get(slot)
                {
                    let mut clone = stack.clone();
                    clone.count = max_stack(&clone);
                    self.carried = Some(clone);
                }
            }
            ClickMode::Throw if self.carried.is_none() => {
                let Some(slot) = slot else { return };
                let Some(current) = self.get(slot) else {
                    return;
                };
                let amount = if button == 0 { 1 } else { current.count };
                if let Some(taken) = self.try_remove(slot, amount, u8::MAX) {
                    dropped.push(taken);
                }
                if button == 1 {
                    // Ctrl+Q keeps throwing while the same item is left in the slot
                    while let Some(current) = self.get(slot).cloned() {
                        if !dropped.last().is_some_and(|last| same_item(last, &current)) {
                            break;
                        }
                        let amount = current.count;
                        match self.try_remove(slot, amount, u8::MAX) {
                            Some(taken) => dropped.push(taken),
                            None => break,
                        }
                    }
                }
            }
            ClickMode::PickupAll => {
                let Some(slot) = slot else { return };
                self.pickup_all(slot, button);
            }
            _ => {}
        }
    }

    fn pickup(&mut self, slot: usize, primary: bool) {
        let clicked = self.get(slot).cloned();
        let carried = self.carried.take();
        match (clicked, carried) {
            (None, Some(carried)) => {
                let amount = if primary { carried.count } else { 1 };
                self.carried = self.safe_insert(slot, carried, amount);
            }
            (None, None) => {}
            (Some(clicked), None) => {
                let amount = if primary {
                    clicked.count
                } else {
                    clicked.count.div_ceil(2)
                };
                self.carried = self.try_remove(slot, amount, u8::MAX);
            }
            (Some(clicked), Some(mut carried)) => {
                if self.may_place(slot, &carried) {
                    if same_item(&clicked, &carried) {
                        let amount = if primary { carried.count } else { 1 };
                        self.carried = self.safe_insert(slot, carried, amount);
                    } else if carried.count <= max_stack(&carried) {
                        // The stacks are swapped
                        self.set(slot, Some(carried));
                        self.carried = Some(clicked);
                    } else {
                        self.carried = Some(carried);
                    }
                } else if same_item(&clicked, &carried) {
                    let room = max_stack(&carried).saturating_sub(carried.count);
                    if let Some(taken) = self.try_remove(slot, clicked.count, room) {
                        carried.count += taken.count;
                    }
                    self.carried = Some(carried);
                } else {
                    self.carried = Some(carried);
                }
            }
        }
    }

    /// The offhand is not a slot of the menu, but number key 40 swaps with it
    fn swap(&mut self, slot: usize, button: i32, dropped: &mut Vec<ItemStack>) {
        let size = self.container_size();
        let source_slot = if button == 40 {
            size + MAIN_SLOTS + HOTBAR_SLOTS
        } else {
            size + MAIN_SLOTS + button as usize
        };
        if source_slot == slot {
            return;
        }
        let source = self.get(source_slot).cloned();
        let target = self.get(slot).cloned();
        match (source, target) {
            (None, None) => {}
            (None, Some(target)) => {
                self.set(slot, None);
                self.set(source_slot, Some(target));
            }
            (Some(mut source), None) => {
                if self.may_place(slot, &source) {
                    let max = max_stack(&source);
                    if source.count > max {
                        let placed = split(&mut source, max);
                        self.set(slot, Some(placed));
                        self.set(source_slot, Some(source));
                    } else {
                        self.set(source_slot, None);
                        self.set(slot, Some(source));
                    }
                }
            }
            (Some(mut source), Some(target)) => {
                if self.may_place(slot, &source) {
                    let max = max_stack(&source);
                    if source.count > max {
                        let placed = split(&mut source, max);
                        self.set(slot, Some(placed));
                        self.set(source_slot, Some(source));
                        // The displaced stack goes into the inventory, or out of it
                        if let Some(rest) = self.add_to_player_inventory(target) {
                            dropped.push(rest);
                        }
                    } else {
                        self.set(source_slot, Some(target));
                        self.set(slot, Some(source));
                    }
                }
            }
        }
    }

    /// `Inventory.add`: hotbar first like the game does, merging stacks before using empty slots.
    /// Returns what did not fit.
    fn add_to_player_inventory(&mut self, mut stack: ItemStack) -> Option<ItemStack> {
        let size = self.container_size();
        let range = size..size + MAIN_SLOTS + HOTBAR_SLOTS;
        // The hotbar comes before the main inventory
        let hotbar_first: Vec<usize> = (size + MAIN_SLOTS..range.end)
            .chain(size..size + MAIN_SLOTS)
            .collect();
        for &slot in &hotbar_first {
            let Some(current) = self.get(slot).cloned() else {
                continue;
            };
            if same_item(&current, &stack) {
                let room = max_stack(&current).saturating_sub(current.count);
                let moved = room.min(stack.count);
                stack.count -= moved;
                self.set(
                    slot,
                    Some(ItemStack {
                        count: current.count + moved,
                        ..current
                    }),
                );
                if stack.count == 0 {
                    return None;
                }
            }
        }
        for &slot in &hotbar_first {
            if self.get(slot).is_none() {
                let max = max_stack(&stack);
                let placed = split(&mut stack, max);
                self.set(slot, Some(placed));
                if stack.count == 0 {
                    return None;
                }
            }
        }
        Some(stack)
    }

    fn pickup_all(&mut self, slot: usize, button: i8) {
        let Some(mut carried) = self.carried.take() else {
            return;
        };
        if self.get(slot).is_none() {
            let total = self.shown_slots();
            let (start, step): (i32, i32) = if button == 0 {
                (0, 1)
            } else {
                (total as i32 - 1, -1)
            };
            for pass in 0..2 {
                let mut i = start;
                while i >= 0 && (i as usize) < total && carried.count < max_stack(&carried) {
                    let idx = i as usize;
                    i += step;
                    let Some(target) = self.get(idx).cloned() else {
                        continue;
                    };
                    if !same_item(&target, &carried) {
                        continue;
                    }
                    if pass == 0 && target.count == max_stack(&target) {
                        continue;
                    }
                    let room = max_stack(&carried) - carried.count;
                    if let Some(taken) = self.try_remove(idx, target.count, room) {
                        carried.count += taken.count;
                    }
                }
            }
        }
        self.carried = Some(carried);
    }

    fn quick_craft(&mut self, slot: Option<usize>, button: i32, dropped: &mut Vec<ItemStack>) {
        let expected = self.quick_craft_status;
        self.quick_craft_status = button & 3;
        if (expected != 1 || self.quick_craft_status != 2) && expected != self.quick_craft_status {
            self.reset_quick_craft();
        } else if self.carried.is_none() {
            self.reset_quick_craft();
        } else if self.quick_craft_status == 0 {
            self.quick_craft_type = (button >> 2) & 3;
            let valid = match self.quick_craft_type {
                0 | 1 => true,
                2 => self.infinite_materials,
                _ => false,
            };
            if valid {
                self.quick_craft_status = 1;
                self.quick_craft_slots.clear();
            } else {
                self.reset_quick_craft();
            }
        } else if self.quick_craft_status == 1 {
            let carried = self.carried.clone().unwrap();
            if let Some(slot) = slot
                && self.can_quick_replace(slot, &carried)
                && self.may_place(slot, &carried)
                && (self.quick_craft_type == 2
                    || carried.count as usize > self.quick_craft_slots.len())
                && !self.quick_craft_slots.contains(&slot)
            {
                self.quick_craft_slots.push(slot);
            }
        } else if self.quick_craft_status == 2 {
            if !self.quick_craft_slots.is_empty() {
                if self.quick_craft_slots.len() == 1 {
                    let slot = self.quick_craft_slots[0];
                    let kind = self.quick_craft_type;
                    self.reset_quick_craft();
                    // A drag over one slot is the same as a click
                    self.click(slot as i32, kind as i8, ClickMode::Pickup, dropped);
                    return;
                }
                let source = self.carried.clone().unwrap();
                let mut remaining = source.count as i32;
                let drag_slots = self.quick_craft_slots.clone();
                for slot in &drag_slots {
                    let carried_count = self.carried.as_ref().map_or(0, |c| c.count);
                    let carried_stack = self.carried.clone().unwrap();
                    if self.can_quick_replace(*slot, &carried_stack)
                        && self.may_place(*slot, &carried_stack)
                        && (self.quick_craft_type == 2
                            || carried_count as usize >= drag_slots.len())
                    {
                        let existing = self.get(*slot).map_or(0, |current| current.count) as i32;
                        let max = max_stack(&source) as i32;
                        let per_slot = match self.quick_craft_type {
                            0 => source.count as i32 / drag_slots.len() as i32,
                            1 => 1,
                            _ => max,
                        };
                        let new_count = (per_slot + existing).min(max);
                        remaining -= new_count - existing;
                        self.set(
                            *slot,
                            Some(ItemStack {
                                count: new_count as u8,
                                ..source.clone()
                            }),
                        );
                    }
                }
                let mut rest = source;
                rest.count = remaining.max(0) as u8;
                self.carried = (rest.count > 0).then_some(rest);
            }
            self.reset_quick_craft();
        } else {
            self.reset_quick_craft();
        }
    }

    /// What happens when the screen is closed: the carried stack goes back into the inventory
    pub fn close(&mut self) -> Option<ItemStack> {
        let carried = self.carried.take()?;
        self.add_to_player_inventory(carried)
    }

    pub fn next_state_id(&mut self) -> i32 {
        self.state_id = (self.state_id + 1) & 0x7FFF;
        self.state_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mchprs_blocks::items::Item;

    fn stack(name: &str, count: u8) -> ItemStack {
        ItemStack {
            item_type: Item::from_name(name).unwrap(),
            count,
            nbt: None,
        }
    }

    fn menu_with(container: &[(usize, ItemStack)], inventory: &[(usize, ItemStack)]) -> Menu {
        let mut inv = Inventory::new(27);
        for (slot, stack) in container {
            inv.set(*slot, Some(stack_to_entry(stack, *slot)));
        }
        let mut player = vec![None; 46];
        for (slot, stack) in inventory {
            player[*slot] = Some(stack.clone());
        }
        Menu::new(
            1,
            BlockPos::new(0, 0, 0),
            ContainerType::Barrel,
            &inv,
            &player,
            false,
        )
    }

    fn count(menu: &Menu, slot: usize) -> u8 {
        menu.get(slot).map_or(0, |stack| stack.count)
    }

    fn click(menu: &mut Menu, slot: i32, button: i8, mode: ClickMode) -> Vec<ItemStack> {
        let mut dropped = Vec::new();
        menu.click(slot, button, mode, &mut dropped);
        dropped
    }

    #[test]
    fn pick_up_and_place_whole_stack() {
        let mut menu = menu_with(&[(0, stack("minecraft:stone", 10))], &[]);
        click(&mut menu, 0, 0, ClickMode::Pickup);
        assert_eq!(menu.carried.as_ref().unwrap().count, 10);
        assert_eq!(count(&menu, 0), 0);
        click(&mut menu, 5, 0, ClickMode::Pickup);
        assert!(menu.carried.is_none());
        assert_eq!(count(&menu, 5), 10);
    }

    #[test]
    fn right_click_takes_half_and_places_one() {
        let mut menu = menu_with(&[(0, stack("minecraft:stone", 11))], &[]);
        click(&mut menu, 0, 1, ClickMode::Pickup);
        assert_eq!(menu.carried.as_ref().unwrap().count, 6);
        assert_eq!(count(&menu, 0), 5);
        click(&mut menu, 1, 1, ClickMode::Pickup);
        assert_eq!(menu.carried.as_ref().unwrap().count, 5);
        assert_eq!(count(&menu, 1), 1);
    }

    #[test]
    fn clicking_the_same_item_merges_up_to_the_stack_size() {
        let mut menu = menu_with(
            &[
                (0, stack("minecraft:stone", 60)),
                (1, stack("minecraft:stone", 10)),
            ],
            &[],
        );
        click(&mut menu, 1, 0, ClickMode::Pickup);
        click(&mut menu, 0, 0, ClickMode::Pickup);
        assert_eq!(count(&menu, 0), 64);
        assert_eq!(menu.carried.as_ref().unwrap().count, 6);
    }

    #[test]
    fn clicking_another_item_swaps_the_stacks() {
        let mut menu = menu_with(
            &[
                (0, stack("minecraft:stone", 3)),
                (1, stack("minecraft:sand", 2)),
            ],
            &[],
        );
        click(&mut menu, 0, 0, ClickMode::Pickup);
        click(&mut menu, 1, 0, ClickMode::Pickup);
        assert_eq!(
            menu.get(1).unwrap().item_type,
            Item::from_name("minecraft:stone").unwrap()
        );
        assert_eq!(
            menu.carried.as_ref().unwrap().item_type,
            Item::from_name("minecraft:sand").unwrap()
        );
    }

    #[test]
    fn shift_click_moves_into_the_container_and_back() {
        // Slot 27 is the first slot of the main inventory of a barrel menu
        let mut menu = menu_with(
            &[],
            &[
                (9, stack("minecraft:stone", 40)),
                (36, stack("minecraft:stone", 30)),
            ],
        );
        click(&mut menu, 27, 0, ClickMode::QuickMove);
        assert_eq!(count(&menu, 0), 40);
        assert_eq!(count(&menu, 27), 0);
        // The hotbar stack merges into the stack that is already there
        click(&mut menu, 27 + 27, 0, ClickMode::QuickMove);
        assert_eq!(count(&menu, 0), 64);
        assert_eq!(count(&menu, 1), 6);

        // From the container to the inventory, which fills the hotbar from the end
        click(&mut menu, 0, 0, ClickMode::QuickMove);
        assert_eq!(count(&menu, 0), 0);
        assert_eq!(count(&menu, 27 + 27 + 8), 64);
    }

    #[test]
    fn shift_click_keeps_the_rest_when_nothing_fits() {
        let mut container = Vec::new();
        for slot in 0..27 {
            container.push((slot, stack("minecraft:sand", 64)));
        }
        let mut menu = menu_with(&container, &[(9, stack("minecraft:stone", 5))]);
        click(&mut menu, 27, 0, ClickMode::QuickMove);
        assert_eq!(count(&menu, 27), 5);
    }

    #[test]
    fn number_key_swaps_with_the_hotbar() {
        let mut menu = menu_with(
            &[(2, stack("minecraft:stone", 7))],
            &[(36 + 3, stack("minecraft:sand", 1))],
        );
        click(&mut menu, 2, 3, ClickMode::Swap);
        assert_eq!(
            menu.get(2).unwrap().item_type,
            Item::from_name("minecraft:sand").unwrap()
        );
        assert_eq!(count(&menu, 27 + 27 + 3), 7);
    }

    #[test]
    fn throw_drops_one_or_the_whole_stack() {
        let mut menu = menu_with(&[(0, stack("minecraft:stone", 5))], &[]);
        let dropped = click(&mut menu, 0, 0, ClickMode::Throw);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].count, 1);
        assert_eq!(count(&menu, 0), 4);
        let dropped = click(&mut menu, 0, 1, ClickMode::Throw);
        assert_eq!(dropped.iter().map(|s| s.count as i32).sum::<i32>(), 4);
        assert_eq!(count(&menu, 0), 0);
    }

    #[test]
    fn clicking_outside_drops_the_carried_stack() {
        let mut menu = menu_with(&[(0, stack("minecraft:stone", 5))], &[]);
        click(&mut menu, 0, 0, ClickMode::Pickup);
        let dropped = click(&mut menu, OUTSIDE_SLOT, 1, ClickMode::Pickup);
        assert_eq!(dropped[0].count, 1);
        assert_eq!(menu.carried.as_ref().unwrap().count, 4);
        let dropped = click(&mut menu, OUTSIDE_SLOT, 0, ClickMode::Pickup);
        assert_eq!(dropped[0].count, 4);
        assert!(menu.carried.is_none());
    }

    #[test]
    fn double_click_collects_the_stack_from_all_slots() {
        let mut menu = menu_with(
            &[
                (0, stack("minecraft:stone", 10)),
                (3, stack("minecraft:stone", 20)),
            ],
            &[(9, stack("minecraft:stone", 5))],
        );
        // Pick up and put down, as the first click of a double click does
        click(&mut menu, 0, 0, ClickMode::Pickup);
        click(&mut menu, 0, 0, ClickMode::Pickup);
        click(&mut menu, 0, 0, ClickMode::Pickup);
        // The second click of the double click on an empty slot gathers everything
        click(&mut menu, 8, 0, ClickMode::PickupAll);
        assert_eq!(menu.carried.as_ref().unwrap().count, 35);
        assert_eq!(count(&menu, 3), 0);
        assert_eq!(count(&menu, 27), 0);
    }

    #[test]
    fn dragging_splits_the_carried_stack_evenly() {
        let mut menu = menu_with(&[(0, stack("minecraft:stone", 10))], &[]);
        click(&mut menu, 0, 0, ClickMode::Pickup);
        // Start the drag with the left button, add three slots, end the drag
        click(&mut menu, OUTSIDE_SLOT, 0, ClickMode::QuickCraft);
        for slot in 1..=3 {
            click(&mut menu, slot, 1, ClickMode::QuickCraft);
        }
        click(&mut menu, OUTSIDE_SLOT, 2, ClickMode::QuickCraft);
        assert_eq!(count(&menu, 1), 3);
        assert_eq!(count(&menu, 2), 3);
        assert_eq!(count(&menu, 3), 3);
        assert_eq!(menu.carried.as_ref().unwrap().count, 1);
    }

    #[test]
    fn right_drag_places_one_item_per_slot() {
        let mut menu = menu_with(&[(0, stack("minecraft:stone", 10))], &[]);
        click(&mut menu, 0, 0, ClickMode::Pickup);
        click(&mut menu, OUTSIDE_SLOT, 4, ClickMode::QuickCraft);
        for slot in 1..=4 {
            click(&mut menu, slot, 5, ClickMode::QuickCraft);
        }
        click(&mut menu, OUTSIDE_SLOT, 6, ClickMode::QuickCraft);
        for slot in 1..=4 {
            assert_eq!(count(&menu, slot), 1);
        }
        assert_eq!(menu.carried.as_ref().unwrap().count, 6);
    }

    #[test]
    fn closing_returns_the_carried_stack_to_the_inventory() {
        let mut menu = menu_with(&[(0, stack("minecraft:stone", 10))], &[]);
        click(&mut menu, 0, 0, ClickMode::Pickup);
        assert!(menu.close().is_none());
        assert_eq!(count(&menu, 27 + 27), 10);
    }

    #[test]
    fn menu_slots_round_trip_into_the_inventories() {
        let mut menu = menu_with(
            &[(0, stack("minecraft:stone", 10))],
            &[(9, stack("minecraft:sand", 2))],
        );
        click(&mut menu, 0, 0, ClickMode::QuickMove);
        let mut player = vec![None; 46];
        player[9] = Some(stack("minecraft:sand", 2));
        let changed = menu.store_player_inventory(&mut player);
        assert_eq!(changed, vec![44]);
        assert_eq!(player[44].as_ref().unwrap().count, 10);
        assert!(menu.container_inventory().is_empty());
    }

    /// Prints the packets of an open screen as hex, so they can be checked against the codecs of
    /// the game
    #[test]
    #[ignore]
    fn dump_menu_packets() {
        use mchprs_network::packets::clientbound::*;
        let menu = menu_with(
            &[(0, stack("minecraft:stone", 10))],
            &[(36, stack("minecraft:redstone", 64))],
        );
        let slot_data = menu
            .slots()
            .iter()
            .map(|slot| slot.as_ref().map(crate::utils::encode_slot_data))
            .collect();
        let packets = [
            COpenScreen {
                window_id: 3,
                window_type: ContainerType::Hopper.window_type(),
                window_title: "Item Hopper".into(),
            }
            .encode(),
            CSetContainerContent {
                window_id: 3,
                state_id: 5,
                slot_data,
                carried_item: Some(crate::utils::encode_slot_data(&stack("minecraft:sand", 7))),
            }
            .encode(),
            CCloseContainer { window_id: 3 }.encode(),
        ];
        for packet in packets {
            let mut bytes = Vec::new();
            packet.write_uncompressed(&mut bytes).unwrap();
            println!(
                "PACKET {}",
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
        }
    }
}
