// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeSet;

use super::PersistentState;

pub const BITCOIN_APP_ID: &str = "0x426974636f696e2057616c6c65740000";
pub const SETTINGS_APP_ID_STR: &str = "0xc192b79230473875f159d4423d74d00f";
pub const SCAN_QR_ACTION_ID: &str = "scan-qr";
pub const LAYOUT_VERSION: u32 = 2;

const DOCK_COLLECTION_ID: &str = "dock";
const NEW_PAGE_COLLECTION_ID: &str = "new-page";
const PAGE_COLLECTION_PREFIX: &str = "page-";
/// Grid slots on the first launcher page when the Bitcoin price chart is shown:
/// the chart takes the top row, leaving two rows of three icons.
const GRAPH_PAGE_CAPACITY: usize = 6;
/// Grid slots on launcher pages without the price chart: three rows of three icons.
const STANDARD_PAGE_CAPACITY: usize = 9;
/// The dock holds a single row of three icons.
const DOCK_CAPACITY: usize = 3;

#[derive(Clone)]
pub struct LauncherConfig {
    pub pages: Vec<LauncherCollection>,
    pub dock: LauncherCollection,
}

/// A grid of numbered slots holding launcher items, gaps allowed.
#[derive(Clone)]
pub struct LauncherCollection {
    capacity: usize,
    pub items: Vec<SlotItem>,
}

#[derive(Clone)]
pub struct SlotItem {
    pub slot: usize,
    pub item: LauncherItem,
}

#[derive(Clone)]
pub struct LauncherItem {
    pub id: String,
    pub label: String,
    pub icon_key: String,
    pub target: LauncherTarget,
    pub enabled: bool,
    pub can_remove: bool,
}

#[derive(Clone)]
pub enum LauncherTarget {
    App { app_id: String },
    Action { action: LauncherAction },
}

#[derive(Clone, Copy)]
pub enum LauncherAction {
    ScanQr,
}

impl LauncherCollection {
    fn new(capacity: usize) -> Self { Self { capacity, items: Vec::new() } }

    /// Claim the saved ids into their saved slots; ids that no longer exist leave a gap.
    fn from_saved_order(remaining: &mut Vec<LauncherItem>, saved_ids: &[String], capacity: usize) -> Self {
        Self {
            capacity,
            items: saved_ids
                .iter()
                .enumerate()
                .filter_map(|(slot, id)| {
                    (!id.is_empty())
                        .then(|| claim_item(remaining, id))
                        .flatten()
                        .map(|item| SlotItem { slot, item })
                })
                .collect(),
        }
    }

    fn is_empty(&self) -> bool { self.items.is_empty() }

    fn is_free(&self, slot: usize) -> bool { self.items.iter().all(|slot_item| slot_item.slot != slot) }

    fn first_free_slot(&self) -> Option<usize> { (0..self.capacity).find(|slot| self.is_free(*slot)) }

    fn remove(&mut self, slot: usize) -> Option<LauncherItem> {
        let index = self.items.iter().position(|slot_item| slot_item.slot == slot)?;
        Some(self.items.remove(index).item)
    }

    /// Place `item` at `slot`, sliding the run between it and the nearest free slot
    /// out of the way. Returns the item pushed off the end when the grid is full.
    fn insert(&mut self, slot: usize, item: LauncherItem) -> Option<LauncherItem> {
        if self.capacity == 0 {
            return Some(item);
        }

        let target = slot.min(self.capacity - 1);
        self.shift_run(target, self.slide_target(target));
        let displaced = self.remove(self.capacity);
        self.items.push(SlotItem { slot: target, item });
        self.sort();
        displaced
    }

    /// The free slot a drop at `slot` slides the run into, or the capacity when there
    /// is none and the last item is pushed off the end.
    fn slide_target(&self, slot: usize) -> usize {
        let target = slot.min(self.capacity.saturating_sub(1));
        (target..self.capacity)
            .find(|slot| self.is_free(*slot))
            .or_else(|| (0..target).rev().find(|slot| self.is_free(*slot)))
            .unwrap_or(self.capacity)
    }

    /// Place `item` at `slot`, handing back whatever sat there when the grid is full.
    fn insert_or_swap(&mut self, slot: usize, item: LauncherItem) -> Option<LauncherItem> {
        if self.capacity == 0 {
            return Some(item);
        }

        let target = slot.min(self.capacity - 1);
        match self.items.iter().position(|slot_item| slot_item.slot == target) {
            Some(index) if self.first_free_slot().is_none() => {
                Some(std::mem::replace(&mut self.items[index].item, item))
            }
            _ => self.insert(target, item),
        }
    }

    /// Move the item at `from` onto `to`, sliding the run between them one slot
    /// toward the vacated one, because that is what the drag preview animates.
    fn reorder(&mut self, from: usize, to: usize) -> bool {
        let Some(index) = self.items.iter().position(|slot_item| slot_item.slot == from) else {
            return false;
        };

        let target = to.min(self.capacity.saturating_sub(1));
        self.shift_run(target, from);
        self.items[index].slot = target;
        self.sort();
        true
    }

    /// Slide the run between `target` and the free slot at `free` one step toward
    /// `free`, leaving `target` empty. `free` is excluded, so an item parked there stays.
    fn shift_run(&mut self, target: usize, free: usize) {
        for slot_item in &mut self.items {
            if (target..free).contains(&slot_item.slot) {
                slot_item.slot += 1;
            } else if (free + 1..=target).contains(&slot_item.slot) {
                slot_item.slot -= 1;
            }
        }
    }

    /// Set the capacity, returning items that no longer fit or share a slot.
    fn resize(&mut self, capacity: usize) -> Vec<LauncherItem> {
        self.capacity = capacity;
        self.sort();

        let mut seen_slots = BTreeSet::new();
        let mut retained = Vec::new();
        let mut overflow = Vec::new();
        for slot_item in self.items.drain(..) {
            if slot_item.slot < self.capacity && seen_slots.insert(slot_item.slot) {
                retained.push(slot_item);
            } else {
                overflow.push(slot_item.item);
            }
        }
        self.items = retained;
        overflow
    }

    fn sort(&mut self) { self.items.sort_by_key(|slot_item| slot_item.slot); }

    /// Item ids by slot, empty strings for gaps: the saved form of a collection.
    fn order(&self) -> Vec<String> {
        let mut ids = vec![
            String::new();
            self.items.iter().map(|slot_item| slot_item.slot).max().map_or(0, |slot| slot + 1)
        ];
        for slot_item in &self.items {
            ids[slot_item.slot] = slot_item.item.id.clone();
        }
        ids
    }
}

impl LauncherConfig {
    /// Build the launcher layout from discovered items and any saved ordering.
    ///
    /// Saved page and dock orders keep their slots (ids that no longer exist are dropped);
    /// the default dock items claim their canonical dock slot when nothing else placed them;
    /// any other item without a saved place fills the first open slot on the first non-full
    /// page, opening a new page when every page is full.
    pub fn from_persistent(persistent: &PersistentState, discovered: Vec<LauncherItem>) -> Self {
        let mut remaining = discovered;

        // Orders saved by an older layout scheme reference stale item ids; ignore
        // them and rebuild from defaults rather than scattering items.
        let (saved_page_orders, saved_dock_order): (&[Vec<String>], &[String]) =
            if persistent.layout_version >= LAYOUT_VERSION {
                (&persistent.page_orders, &persistent.dock_order)
            } else {
                (&[], &[])
            };

        // Ids past the last dock slot have nowhere to land; unclaimed, they fall through to a page.
        let saved_dock_slots = &saved_dock_order[..saved_dock_order.len().min(DOCK_CAPACITY)];
        let mut dock = LauncherCollection::from_saved_order(&mut remaining, saved_dock_slots, DOCK_CAPACITY);

        // Dropping the empty pages shifts the rest up, so these capacities are a
        // guess that the compaction below fixes.
        let mut pages: Vec<LauncherCollection> = saved_page_orders
            .iter()
            .enumerate()
            .map(|(index, saved_ids)| {
                LauncherCollection::from_saved_order(&mut remaining, saved_ids, page_capacity_for(index))
            })
            .filter(|page| !page.is_empty())
            .collect();
        Self::compact_pages(&mut pages);

        // Settings, Bitcoin, and Scan QR live in the dock by default: when neither
        // the saved dock nor a saved page placed them, insert each at its canonical
        // dock position (covers fresh layouts and layouts saved by older builds).
        for (slot, id) in [SETTINGS_APP_ID_STR, BITCOIN_APP_ID, SCAN_QR_ACTION_ID].into_iter().enumerate() {
            if dock.items.len() >= DOCK_CAPACITY {
                break;
            }
            if let Some(item) = claim_item(&mut remaining, id) {
                let displaced = dock.insert(slot, item);
                debug_assert!(displaced.is_none(), "the dock has a free slot");
            }
        }

        for item in remaining {
            match pages
                .iter()
                .enumerate()
                .find_map(|(index, page)| page.first_free_slot().map(|slot| (index, slot)))
            {
                Some((page_index, slot)) => Self::insert_item_into_pages(&mut pages, page_index, slot, item),
                None => {
                    let mut page = LauncherCollection::new(page_capacity_for(pages.len()));
                    page.items.push(SlotItem { slot: 0, item });
                    pages.push(page);
                }
            }
        }

        Self::compact_pages(&mut pages);

        Self { pages, dock }
    }

    pub fn item_by_id(&self, item_id: &str) -> Option<&LauncherItem> {
        self.pages
            .iter()
            .chain(std::iter::once(&self.dock))
            .flat_map(|collection| collection.items.iter().map(|slot_item| &slot_item.item))
            .find(|item| item.id == item_id)
    }

    pub fn move_item(
        &mut self,
        source_collection_id: &str,
        from_index: usize,
        target_collection_id: &str,
        to_index: usize,
    ) -> bool {
        if source_collection_id == target_collection_id {
            let Some(collection) = self.collection_mut(source_collection_id) else {
                return false;
            };
            if !collection.reorder(from_index, to_index) {
                return false;
            }
            self.compact_page_list();
            return true;
        }

        let Some(item) = self.remove_item(source_collection_id, from_index) else {
            return false;
        };

        match self.place_item(target_collection_id, to_index, item) {
            Ok(displaced) => {
                // The drag just vacated its slot, so the swapped-out icon lands
                // where the dragged one came from.
                if let Some(displaced) = displaced {
                    let restored = self.place_item(source_collection_id, from_index, displaced);
                    debug_assert!(restored.is_ok(), "the drag source outlives the move");
                }
                self.compact_page_list();
                true
            }
            Err(item) => {
                let _ = self.place_item(source_collection_id, from_index, item);
                false
            }
        }
    }

    pub fn sync_persistent(&self, persistent: &mut PersistentState) {
        persistent.page_orders = self.pages.iter().map(LauncherCollection::order).collect();
        persistent.dock_order = self.dock.order();
        persistent.layout_version = LAYOUT_VERSION;
    }

    /// Where a drop at `slot` slides the run to: a free slot, or the capacity when the
    /// last item is pushed off the end. None when nothing slides, either because a full
    /// dock swaps the hovered icon out or because there is no such collection.
    pub fn drop_slide_target(&self, collection_id: &str, slot: usize) -> Option<usize> {
        let collection = self.collection(collection_id)?;
        if collection_id == DOCK_COLLECTION_ID && collection.first_free_slot().is_none() {
            return None;
        }

        Some(collection.slide_target(slot))
    }

    /// The icon a drop at `slot` pushes out, which only a full dock does.
    pub fn displaced_item(&self, collection_id: &str, slot: usize) -> Option<&LauncherItem> {
        if collection_id != DOCK_COLLECTION_ID || self.dock.first_free_slot().is_some() {
            return None;
        }

        let target = slot.min(DOCK_CAPACITY - 1);
        self.dock.items.iter().find(|slot_item| slot_item.slot == target).map(|slot_item| &slot_item.item)
    }

    fn collection(&self, collection_id: &str) -> Option<&LauncherCollection> {
        if collection_id == DOCK_COLLECTION_ID {
            return Some(&self.dock);
        }

        let page_index = page_index_from_collection_id(collection_id)?;
        self.pages.get(page_index)
    }

    fn collection_mut(&mut self, collection_id: &str) -> Option<&mut LauncherCollection> {
        if collection_id == DOCK_COLLECTION_ID {
            return Some(&mut self.dock);
        }

        let page_index = page_index_from_collection_id(collection_id)?;
        self.pages.get_mut(page_index)
    }

    fn remove_item(&mut self, collection_id: &str, slot: usize) -> Option<LauncherItem> {
        self.collection_mut(collection_id)?.remove(slot)
    }

    /// Place `item` at `slot` of the named collection. A full dock hands back the
    /// icon it swapped out, having nowhere to spill; pages spill onto the next page.
    /// `Err` returns the item when the collection does not exist.
    fn place_item(
        &mut self,
        collection_id: &str,
        slot: usize,
        item: LauncherItem,
    ) -> Result<Option<LauncherItem>, LauncherItem> {
        if collection_id == DOCK_COLLECTION_ID {
            return Ok(self.dock.insert_or_swap(slot, item));
        }

        if collection_id == NEW_PAGE_COLLECTION_ID {
            let page_index = self.pages.len();
            Self::insert_item_into_pages(&mut self.pages, page_index, slot, item);
            return Ok(None);
        }

        match page_index_from_collection_id(collection_id).filter(|index| *index < self.pages.len()) {
            Some(page_index) => {
                Self::insert_item_into_pages(&mut self.pages, page_index, slot, item);
                Ok(None)
            }
            None => Err(item),
        }
    }

    fn compact_page_list(&mut self) { Self::compact_pages(&mut self.pages); }

    fn compact_pages(pages: &mut Vec<LauncherCollection>) {
        loop {
            pages.retain(|page| !page.is_empty());

            if pages.is_empty() {
                pages.push(LauncherCollection::new(page_capacity_for(0)));
                return;
            }

            // A page's capacity follows its index, which dropping the empty pages may
            // have shifted.
            let mut page_index = 0;
            while page_index < pages.len() {
                let overflow = pages[page_index].resize(page_capacity_for(page_index));
                for item in overflow.into_iter().rev() {
                    Self::insert_item_into_pages(pages, page_index + 1, 0, item);
                }
                page_index += 1;
            }

            if pages.iter().all(|page| !page.is_empty()) {
                return;
            }
        }
    }

    /// Place `item` at `slot` of `page_index`, cascading the overflow onto later
    /// pages and opening new ones as needed.
    fn insert_item_into_pages(
        pages: &mut Vec<LauncherCollection>,
        mut page_index: usize,
        mut slot: usize,
        mut item: LauncherItem,
    ) {
        loop {
            while page_index >= pages.len() {
                pages.push(LauncherCollection::new(page_capacity_for(pages.len())));
            }

            debug_assert_eq!(pages[page_index].capacity, page_capacity_for(page_index), "stale capacity");
            match pages[page_index].insert(slot, item) {
                Some(overflow) => {
                    item = overflow;
                    page_index += 1;
                    slot = 0;
                }
                None => break,
            }
        }
    }
}

pub fn page_collection_id(page_index: usize) -> String { format!("{PAGE_COLLECTION_PREFIX}{page_index}") }

fn page_capacity_for(page_index: usize) -> usize {
    if page_index == 0 {
        GRAPH_PAGE_CAPACITY
    } else {
        STANDARD_PAGE_CAPACITY
    }
}

fn claim_item(remaining: &mut Vec<LauncherItem>, id: &str) -> Option<LauncherItem> {
    let index = remaining.iter().position(|item| item.id == id)?;
    Some(remaining.remove(index))
}

fn page_index_from_collection_id(collection_id: &str) -> Option<usize> {
    collection_id.strip_prefix(PAGE_COLLECTION_PREFIX).and_then(|suffix| suffix.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_item(id: &str) -> LauncherItem {
        LauncherItem {
            id: id.to_string(),
            label: id.to_string(),
            icon_key: String::new(),
            target: LauncherTarget::App { app_id: id.to_string() },
            enabled: true,
            can_remove: false,
        }
    }

    fn slot_item_ids(items: &[SlotItem]) -> Vec<&str> {
        items.iter().map(|slot_item| slot_item.item.id.as_str()).collect()
    }

    fn slot_item_slots(items: &[SlotItem]) -> Vec<usize> {
        items.iter().map(|slot_item| slot_item.slot).collect()
    }

    fn test_page(items: &[&str]) -> LauncherCollection {
        test_sparse_page(&items.iter().enumerate().map(|(slot, id)| (slot, *id)).collect::<Vec<_>>())
    }

    fn test_sparse_page(items: &[(usize, &str)]) -> LauncherCollection {
        let mut page = LauncherCollection::new(GRAPH_PAGE_CAPACITY);
        page.items = items.iter().map(|(slot, id)| SlotItem { slot: *slot, item: test_item(id) }).collect();
        page
    }

    fn test_dock(items: &[(usize, &str)]) -> LauncherCollection {
        let mut dock = LauncherCollection::new(DOCK_CAPACITY);
        dock.items = items.iter().map(|(slot, id)| SlotItem { slot: *slot, item: test_item(id) }).collect();
        dock
    }

    /// Compacting stamps each page with the capacity its index calls for, the
    /// same way a rebuilt layout gets one.
    fn test_launcher(pages: Vec<LauncherCollection>, dock: &[&str]) -> LauncherConfig {
        let mut launcher = LauncherConfig {
            pages,
            dock: test_dock(&dock.iter().enumerate().map(|(slot, id)| (slot, *id)).collect::<Vec<_>>()),
        };
        LauncherConfig::compact_pages(&mut launcher.pages);
        launcher
    }

    #[test]
    fn fresh_layout_claims_default_dock_and_fills_pages() {
        let persistent = PersistentState::default();
        let discovered = vec![
            test_item("a"),
            test_item(SETTINGS_APP_ID_STR),
            test_item(BITCOIN_APP_ID),
            test_item(SCAN_QR_ACTION_ID),
            test_item("b"),
        ];

        let config = LauncherConfig::from_persistent(&persistent, discovered);

        assert_eq!(
            slot_item_ids(&config.dock.items),
            [SETTINGS_APP_ID_STR, BITCOIN_APP_ID, SCAN_QR_ACTION_ID]
        );
        assert_eq!(slot_item_slots(&config.dock.items), [0, 1, 2]);
        assert_eq!(config.pages.len(), 1);
        assert_eq!(slot_item_ids(&config.pages[0].items), ["a", "b"]);
    }

    #[test]
    fn unplaced_default_dock_items_claim_canonical_dock_slots() {
        // Layout saved by an older build: only the scan action's id survived
        // the item-id scheme change, so Settings and Bitcoin are unplaced.
        let persistent = PersistentState {
            dock_order: vec![SCAN_QR_ACTION_ID.to_string()],
            page_orders: vec![vec!["a".to_string()]],
            layout_version: LAYOUT_VERSION,
            ..Default::default()
        };
        let discovered = vec![
            test_item("a"),
            test_item(SCAN_QR_ACTION_ID),
            test_item(SETTINGS_APP_ID_STR),
            test_item(BITCOIN_APP_ID),
        ];

        let config = LauncherConfig::from_persistent(&persistent, discovered);

        assert_eq!(
            slot_item_ids(&config.dock.items),
            [SETTINGS_APP_ID_STR, BITCOIN_APP_ID, SCAN_QR_ACTION_ID]
        );
        assert_eq!(slot_item_slots(&config.dock.items), [0, 1, 2]);
        assert_eq!(slot_item_ids(&config.pages[0].items), ["a"]);
    }

    #[test]
    fn stale_layout_version_resets_to_defaults() {
        // Orders written by an older build are ignored entirely.
        let persistent = PersistentState {
            dock_order: vec!["a".to_string()],
            page_orders: vec![vec![SCAN_QR_ACTION_ID.to_string()]],
            layout_version: 0,
            ..Default::default()
        };
        let discovered = vec![
            test_item("a"),
            test_item(SETTINGS_APP_ID_STR),
            test_item(BITCOIN_APP_ID),
            test_item(SCAN_QR_ACTION_ID),
        ];

        let config = LauncherConfig::from_persistent(&persistent, discovered);

        assert_eq!(
            slot_item_ids(&config.dock.items),
            [SETTINGS_APP_ID_STR, BITCOIN_APP_ID, SCAN_QR_ACTION_ID]
        );
        assert_eq!(slot_item_slots(&config.dock.items), [0, 1, 2]);
        assert_eq!(slot_item_ids(&config.pages[0].items), ["a"]);
    }

    #[test]
    fn default_dock_items_stay_where_the_user_put_them() {
        // The user deliberately moved Settings onto a page; it must not be
        // pulled back into the dock.
        let persistent = PersistentState {
            dock_order: vec![BITCOIN_APP_ID.to_string(), SCAN_QR_ACTION_ID.to_string()],
            page_orders: vec![vec![SETTINGS_APP_ID_STR.to_string()]],
            layout_version: LAYOUT_VERSION,
            ..Default::default()
        };
        let discovered =
            vec![test_item(SETTINGS_APP_ID_STR), test_item(BITCOIN_APP_ID), test_item(SCAN_QR_ACTION_ID)];

        let config = LauncherConfig::from_persistent(&persistent, discovered);

        assert_eq!(slot_item_ids(&config.dock.items), [BITCOIN_APP_ID, SCAN_QR_ACTION_ID]);
        assert_eq!(slot_item_slots(&config.dock.items), [0, 1]);
        assert_eq!(slot_item_ids(&config.pages[0].items), [SETTINGS_APP_ID_STR]);
    }

    #[test]
    fn new_apps_fill_first_open_slot_on_first_non_full_page() {
        let persistent = PersistentState {
            dock_order: vec!["d1".to_string()],
            page_orders: vec![
                vec!["a".to_string(), "b".to_string(), "c".to_string()], // 3 free slots
                vec!["e".to_string()],
            ],
            layout_version: LAYOUT_VERSION,
            ..Default::default()
        };
        let discovered = vec![
            test_item("a"),
            test_item("b"),
            test_item("c"),
            test_item("d1"),
            test_item("e"),
            test_item("new"),
        ];

        let config = LauncherConfig::from_persistent(&persistent, discovered);

        assert_eq!(slot_item_ids(&config.dock.items), ["d1"]);
        assert_eq!(slot_item_slots(&config.dock.items), [0]);
        assert_eq!(slot_item_ids(&config.pages[0].items), ["a", "b", "c", "new"]);
        assert_eq!(slot_item_ids(&config.pages[1].items), ["e"]);
    }

    #[test]
    fn overflow_when_all_pages_full_opens_a_new_page() {
        let full_page: Vec<String> = (0..GRAPH_PAGE_CAPACITY).map(|i| format!("app-{i}")).collect();
        let persistent = PersistentState {
            page_orders: vec![full_page.clone()],
            layout_version: LAYOUT_VERSION,
            ..Default::default()
        };
        let mut discovered: Vec<LauncherItem> = full_page.iter().map(|id| test_item(id)).collect();
        discovered.push(test_item("overflow"));

        let config = LauncherConfig::from_persistent(&persistent, discovered);

        assert_eq!(config.pages.len(), 2);
        assert_eq!(slot_item_ids(&config.pages[1].items), ["overflow"]);
    }

    #[test]
    fn regular_pages_hold_three_rows_before_overflowing() {
        let full_first_page: Vec<String> = (0..GRAPH_PAGE_CAPACITY).map(|i| format!("first-{i}")).collect();
        let almost_full_regular_page: Vec<String> =
            (0..STANDARD_PAGE_CAPACITY - 1).map(|i| format!("regular-{i}")).collect();
        let persistent = PersistentState {
            page_orders: vec![full_first_page.clone(), almost_full_regular_page.clone()],
            layout_version: LAYOUT_VERSION,
            ..Default::default()
        };
        let mut discovered: Vec<LauncherItem> =
            full_first_page.iter().chain(almost_full_regular_page.iter()).map(|id| test_item(id)).collect();
        discovered.push(test_item("new"));

        let config = LauncherConfig::from_persistent(&persistent, discovered);

        assert_eq!(config.pages.len(), 2);
        assert_eq!(config.pages[1].items.len(), STANDARD_PAGE_CAPACITY);
        assert_eq!(config.pages[1].items.last().map(|slot_item| slot_item.item.id.as_str()), Some("new"));
    }

    #[test]
    fn saved_overfull_pages_are_rebalanced_on_rebuild() {
        let persistent = PersistentState {
            page_orders: vec![
                vec![
                    "a".to_string(),
                    "b".to_string(),
                    "c".to_string(),
                    "d".to_string(),
                    "e".to_string(),
                    "f".to_string(),
                    "g".to_string(),
                    "h".to_string(),
                ],
                vec!["i".to_string()],
            ],
            layout_version: LAYOUT_VERSION,
            ..Default::default()
        };
        let discovered = ["a", "b", "c", "d", "e", "f", "g", "h", "i"].into_iter().map(test_item).collect();

        let config = LauncherConfig::from_persistent(&persistent, discovered);

        assert_eq!(slot_item_ids(&config.pages[0].items), ["a", "b", "c", "d", "e", "f"]);
        assert_eq!(slot_item_ids(&config.pages[1].items), ["g", "h", "i"]);
    }

    #[test]
    fn moving_item_to_full_page_pushes_last_item_to_next_page() {
        let mut launcher =
            test_launcher(vec![test_page(&["a", "b", "c", "d", "e", "f"]), test_page(&["g", "h"])], &[]);

        assert!(launcher.move_item("page-1", 1, "page-0", 2));

        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["a", "b", "h", "c", "d", "e"]);
        assert_eq!(slot_item_ids(&launcher.pages[1].items), ["f", "g"]);
    }

    #[test]
    fn drop_preview_slides_toward_a_gap_on_either_side() {
        let launcher = test_launcher(vec![test_page(&["a"])], &["d0", "d1"]);

        // Gap at dock slot 2, so a drop on slot 0 slides the run right into it.
        assert_eq!(launcher.drop_slide_target("dock", 0), Some(2));

        let launcher =
            LauncherConfig { pages: vec![test_page(&["a"])], dock: test_dock(&[(1, "d1"), (2, "d2")]) };

        assert_eq!(launcher.drop_slide_target("dock", 2), Some(0));
    }

    #[test]
    fn only_a_full_dock_reports_a_displaced_icon() {
        let launcher = test_launcher(vec![test_page(&["a"])], &["d0", "d1", "d2"]);

        assert_eq!(launcher.displaced_item("dock", 1).map(|item| item.id.as_str()), Some("d1"));
        assert!(launcher.displaced_item("page-0", 0).is_none());

        let launcher = test_launcher(vec![test_page(&["a"])], &["d0", "d1"]);

        assert!(launcher.displaced_item("dock", 1).is_none());
    }

    #[test]
    fn drop_preview_reports_a_swap_on_a_full_dock_and_an_overflow_on_a_full_page() {
        let launcher = test_launcher(vec![test_page(&["a", "b", "c", "d", "e", "f"])], &["d0", "d1", "d2"]);

        assert_eq!(launcher.drop_slide_target("dock", 1), None);
        assert_eq!(launcher.drop_slide_target("page-0", 1), Some(GRAPH_PAGE_CAPACITY));
    }

    #[test]
    fn dragging_onto_the_last_slot_of_a_full_page_keeps_every_icon_on_it() {
        let mut launcher = test_launcher(vec![test_page(&["a", "b", "c", "d", "e", "f"])], &[]);

        assert!(launcher.move_item("page-0", 0, "page-0", GRAPH_PAGE_CAPACITY - 1));

        assert_eq!(launcher.pages.len(), 1);
        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["b", "c", "d", "e", "f", "a"]);
        assert_eq!(slot_item_slots(&launcher.pages[0].items), [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn reordering_forward_over_a_gap_slides_every_slot_between() {
        let mut launcher = test_launcher(vec![test_sparse_page(&[(0, "a"), (1, "b"), (3, "c")])], &[]);

        assert!(launcher.move_item("page-0", 0, "page-0", 3));

        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["b", "c", "a"]);
        assert_eq!(slot_item_slots(&launcher.pages[0].items), [0, 2, 3]);
    }

    #[test]
    fn reordering_back_over_a_gap_slides_every_slot_between() {
        let mut launcher = test_launcher(vec![test_sparse_page(&[(0, "a"), (2, "b"), (3, "c")])], &[]);

        assert!(launcher.move_item("page-0", 3, "page-0", 0));

        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["c", "a", "b"]);
        assert_eq!(slot_item_slots(&launcher.pages[0].items), [0, 1, 3]);
    }

    #[test]
    fn moving_item_to_occupied_slot_shifts_right_when_space_allows() {
        let mut launcher = test_launcher(vec![test_sparse_page(&[(0, "a"), (1, "b"), (3, "c")])], &["x"]);

        assert!(launcher.move_item("dock", 0, "page-0", 1));

        assert!(launcher.dock.is_empty());
        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["a", "x", "b", "c"]);
        assert_eq!(slot_item_slots(&launcher.pages[0].items), [0, 1, 2, 3]);
    }

    #[test]
    fn moving_item_to_occupied_end_slot_shifts_left_when_space_allows() {
        let mut launcher = test_launcher(
            vec![test_sparse_page(&[(0, "a"), (1, "b"), (3, "c"), (4, "d"), (5, "corner")])],
            &["x"],
        );

        assert!(launcher.move_item("dock", 0, "page-0", GRAPH_PAGE_CAPACITY - 1));

        assert!(launcher.dock.is_empty());
        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["a", "b", "c", "d", "corner", "x"]);
        assert_eq!(slot_item_slots(&launcher.pages[0].items), [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn moving_item_within_page_to_empty_row_preserves_target_slot() {
        let mut launcher = test_launcher(vec![test_page(&["a", "b"])], &[]);

        assert!(launcher.move_item("page-0", 1, "page-0", 4));

        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["a", "b"]);
        assert_eq!(slot_item_slots(&launcher.pages[0].items), [0, 4]);
        assert_eq!(
            launcher.pages.first().map(LauncherCollection::order),
            Some(vec!["a".to_string(), String::new(), String::new(), String::new(), "b".to_string()])
        );
    }

    #[test]
    fn sparse_page_slots_survive_layout_rebuild() {
        let persistent = PersistentState {
            page_orders: vec![vec![
                "a".to_string(),
                String::new(),
                String::new(),
                String::new(),
                "b".to_string(),
            ]],
            layout_version: LAYOUT_VERSION,
            ..Default::default()
        };
        let discovered = vec![test_item("a"), test_item("b")];

        let config = LauncherConfig::from_persistent(&persistent, discovered);

        assert_eq!(slot_item_ids(&config.pages[0].items), ["a", "b"]);
        assert_eq!(slot_item_slots(&config.pages[0].items), [0, 4]);
    }

    #[test]
    fn sparse_page_slots_survive_save_and_refresh_cycle() {
        let mut launcher = test_launcher(vec![test_page(&["a", "b"])], &[]);

        assert!(launcher.move_item("page-0", 1, "page-0", 4));

        let mut persistent = PersistentState::default();
        launcher.sync_persistent(&mut persistent);
        let config = LauncherConfig::from_persistent(&persistent, vec![test_item("a"), test_item("b")]);

        assert_eq!(slot_item_ids(&config.pages[0].items), ["a", "b"]);
        assert_eq!(slot_item_slots(&config.pages[0].items), [0, 4]);
    }

    #[test]
    fn moving_item_between_pages_to_empty_row_preserves_target_slot() {
        let mut launcher = test_launcher(vec![test_page(&["a", "b"]), test_page(&["x"])], &[]);

        assert!(launcher.move_item("page-0", 1, "page-1", 4));

        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["a"]);
        assert_eq!(slot_item_slots(&launcher.pages[0].items), [0]);
        assert_eq!(slot_item_ids(&launcher.pages[1].items), ["x", "b"]);
        assert_eq!(slot_item_slots(&launcher.pages[1].items), [0, 4]);
    }

    #[test]
    fn moving_item_to_new_page_collection_appends_page() {
        let mut launcher = test_launcher(vec![test_page(&["a", "b"])], &[]);

        assert!(launcher.move_item("page-0", 1, NEW_PAGE_COLLECTION_ID, 0));

        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["a"]);
        assert_eq!(slot_item_ids(&launcher.pages[1].items), ["b"]);
        assert_eq!(slot_item_slots(&launcher.pages[1].items), [0]);
    }

    #[test]
    fn moving_dock_item_to_new_page_collection_appends_page() {
        let mut launcher = test_launcher(vec![test_page(&["a"])], &["x"]);

        assert!(launcher.move_item("dock", 0, NEW_PAGE_COLLECTION_ID, 0));

        assert!(launcher.dock.is_empty());
        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["a"]);
        assert_eq!(slot_item_ids(&launcher.pages[1].items), ["x"]);
    }

    #[test]
    fn moving_item_to_empty_dock_slot_preserves_target_slot() {
        let mut launcher = test_launcher(vec![test_page(&["a"])], &["dock"]);

        assert!(launcher.move_item("page-0", 0, "dock", 2));

        assert_eq!(slot_item_ids(&launcher.dock.items), ["dock", "a"]);
        assert_eq!(slot_item_slots(&launcher.dock.items), [0, 2]);
        assert_eq!(launcher.dock.order(), vec!["dock".to_string(), String::new(), "a".to_string()]);
    }

    #[test]
    fn sparse_dock_slots_survive_layout_rebuild() {
        let persistent = PersistentState {
            dock_order: vec!["dock".to_string(), String::new(), "a".to_string()],
            layout_version: LAYOUT_VERSION,
            ..Default::default()
        };

        let config = LauncherConfig::from_persistent(&persistent, vec![test_item("dock"), test_item("a")]);

        assert_eq!(slot_item_ids(&config.dock.items), ["dock", "a"]);
        assert_eq!(slot_item_slots(&config.dock.items), [0, 2]);
    }

    #[test]
    fn moving_dock_item_to_occupied_slot_shifts_toward_empty_slot() {
        let mut launcher =
            LauncherConfig { pages: vec![test_page(&[])], dock: test_dock(&[(0, "a"), (2, "b")]) };

        assert!(launcher.move_item("dock", 0, "dock", 2));

        assert_eq!(slot_item_ids(&launcher.dock.items), ["b", "a"]);
        assert_eq!(slot_item_slots(&launcher.dock.items), [1, 2]);
    }

    #[test]
    fn moving_last_item_off_page_removes_empty_page() {
        let mut launcher = test_launcher(vec![test_page(&["a"]), test_page(&["b"])], &[]);

        assert!(launcher.move_item("page-0", 0, "page-1", 1));

        assert_eq!(launcher.pages.len(), 1);
        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["b", "a"]);
        assert_eq!(slot_item_slots(&launcher.pages[0].items), [0, 1]);
    }

    #[test]
    fn saved_sparse_layout_does_not_keep_empty_leading_page() {
        let mut sparse_page = vec![String::new(); STANDARD_PAGE_CAPACITY];
        sparse_page[STANDARD_PAGE_CAPACITY - 1] = "a".to_string();
        let persistent = PersistentState {
            page_orders: vec![sparse_page],
            layout_version: LAYOUT_VERSION,
            ..Default::default()
        };

        let config = LauncherConfig::from_persistent(&persistent, vec![test_item("a")]);

        assert_eq!(config.pages.len(), 1);
        assert_eq!(slot_item_ids(&config.pages[0].items), ["a"]);
        assert_eq!(slot_item_slots(&config.pages[0].items), [0]);
    }

    #[test]
    fn moving_item_to_full_page_end_slot_still_places_item_on_target_page() {
        let mut launcher = test_launcher(vec![test_page(&["a", "b", "c", "d", "e", "f"])], &["x"]);

        assert!(launcher.move_item("dock", 0, "page-0", GRAPH_PAGE_CAPACITY));

        assert!(launcher.dock.is_empty());
        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["a", "b", "c", "d", "e", "x"]);
        assert_eq!(slot_item_ids(&launcher.pages[1].items), ["f"]);
    }

    #[test]
    fn moving_item_to_full_page_cascades_through_full_pages() {
        let mut launcher = test_launcher(
            vec![
                test_page(&["a0", "a1", "a2", "a3", "a4", "a5"]),
                test_page(&["b0", "b1", "b2", "b3", "b4", "b5", "b6", "b7", "b8"]),
            ],
            &["x"],
        );

        assert!(launcher.move_item("dock", 0, "page-0", 0));

        assert!(launcher.dock.is_empty());
        assert_eq!(slot_item_ids(&launcher.pages[0].items), ["x", "a0", "a1", "a2", "a3", "a4"]);
        assert_eq!(
            slot_item_ids(&launcher.pages[1].items),
            ["a5", "b0", "b1", "b2", "b3", "b4", "b5", "b6", "b7"]
        );
        assert_eq!(slot_item_ids(&launcher.pages[2].items), ["b8"]);
    }

    #[test]
    fn removed_apps_drop_from_saved_layout() {
        let persistent = PersistentState {
            dock_order: vec!["gone-dock".to_string(), "kept".to_string()],
            page_orders: vec![
                vec!["gone".to_string(), "still-here".to_string()],
                vec!["gone-entirely".to_string()],
            ],
            layout_version: LAYOUT_VERSION,
            ..Default::default()
        };
        let discovered = vec![test_item("kept"), test_item("still-here")];

        let config = LauncherConfig::from_persistent(&persistent, discovered);

        assert_eq!(slot_item_ids(&config.dock.items), ["kept"]);
        assert_eq!(slot_item_slots(&config.dock.items), [1]);
        assert_eq!(config.pages.len(), 1);
        assert_eq!(slot_item_ids(&config.pages[0].items), ["still-here"]);
    }
}
