// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeSet;

use super::PersistentState;

pub(crate) const BITCOIN_APP_ID: &str = "0x426974636f696e2057616c6c65740000";
pub(crate) const SETTINGS_APP_ID_STR: &str = "0xc192b79230473875f159d4423d74d00f";
pub(crate) const SCAN_QR_ACTION_ID: &str = "scan-qr";
pub(crate) const LAYOUT_VERSION: u32 = 2;

const DOCK_COLLECTION_ID: &str = "dock";
const PAGE_COLLECTION_PREFIX: &str = "page-";
/// Grid slots on the first launcher page when the Bitcoin price chart is shown:
/// the chart takes the top row, leaving two rows of three icons.
const GRAPH_PAGE_CAPACITY: usize = 6;
/// Grid slots on launcher pages without the price chart: three rows of three icons.
const STANDARD_PAGE_CAPACITY: usize = 9;
/// The dock holds a single row of three icons.
const DOCK_CAPACITY: usize = 3;

#[derive(Clone)]
pub(crate) struct LauncherConfig {
    pub(crate) pages: Vec<LauncherPage>,
    pub(crate) dock: Vec<LauncherItem>,
}

#[derive(Clone)]
pub(crate) struct LauncherPage {
    pub(crate) items: Vec<LauncherPageItem>,
}

#[derive(Clone)]
pub(crate) struct LauncherPageItem {
    pub(crate) slot: usize,
    pub(crate) item: LauncherItem,
}

#[derive(Clone)]
pub(crate) struct LauncherItem {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) icon_key: String,
    pub(crate) target: LauncherTarget,
    pub(crate) enabled: bool,
    pub(crate) can_remove: bool,
}

#[derive(Clone)]
pub(crate) enum LauncherTarget {
    App { app_id: String },
    Action { action: LauncherAction },
}

#[derive(Clone, Copy)]
pub(crate) enum LauncherAction {
    ScanQr,
}

impl LauncherConfig {
    /// Build the launcher layout from discovered items and any saved ordering.
    ///
    /// Saved page and dock orders keep their order (ids that no longer exist are dropped);
    /// the default dock items claim their canonical dock slot when nothing else placed them;
    /// any other item without a saved place fills the first open slot on the first non-full
    /// page, opening a new page when every page is full.
    pub(crate) fn from_persistent(persistent: &PersistentState, discovered: Vec<LauncherItem>) -> Self {
        let mut remaining = discovered;
        let mut pages: Vec<LauncherPage> = Vec::new();
        let mut dock: Vec<LauncherItem> = Vec::new();

        // Orders saved by an older layout scheme reference stale item ids; ignore
        // them and rebuild from defaults rather than scattering items.
        let (saved_page_orders, saved_dock_order): (&[Vec<String>], &[String]) =
            if persistent.layout_version >= LAYOUT_VERSION {
                (&persistent.page_orders, &persistent.dock_order)
            } else {
                (&[], &[])
            };

        for id in saved_dock_order {
            if let Some(item) = claim_item(&mut remaining, id) {
                dock.push(item);
            }
        }

        for saved_ids in saved_page_orders {
            let items: Vec<LauncherPageItem> = saved_ids
                .iter()
                .enumerate()
                .filter_map(|(slot, id)| {
                    (!id.is_empty())
                        .then(|| claim_item(&mut remaining, id))
                        .flatten()
                        .map(|item| LauncherPageItem { slot, item })
                })
                .collect();
            if !items.is_empty() {
                pages.push(LauncherPage { items });
            }
        }
        Self::enforce_page_capacity(&mut pages);

        // Settings, Bitcoin, and Scan QR live in the dock by default: when neither
        // the saved dock nor a saved page placed them, insert each at its canonical
        // dock position (covers fresh layouts and layouts saved by older builds).
        for (index, id) in [SETTINGS_APP_ID_STR, BITCOIN_APP_ID, SCAN_QR_ACTION_ID].into_iter().enumerate() {
            if dock.len() >= DOCK_CAPACITY {
                break;
            }
            if let Some(item) = claim_item(&mut remaining, id) {
                let at = index.min(dock.len());
                dock.insert(at, item);
            }
        }

        for item in remaining {
            match pages.iter().enumerate().find_map(|(index, page)| {
                first_open_page_slot(page, page_capacity_for(index)).map(|slot| (index, slot))
            }) {
                Some((page_index, slot)) => Self::insert_item_into_pages(&mut pages, page_index, slot, item),
                None => {
                    pages.push(LauncherPage { items: vec![LauncherPageItem { slot: 0, item }] });
                }
            }
        }

        if pages.is_empty() {
            pages.push(LauncherPage { items: Vec::new() });
        }

        Self { pages, dock }
    }

    pub(crate) fn item_by_id(&self, item_id: &str) -> Option<&LauncherItem> {
        self.pages
            .iter()
            .flat_map(|page| page.items.iter().map(|page_item| &page_item.item))
            .chain(self.dock.iter())
            .find(|item| item.id == item_id)
    }

    pub(crate) fn move_item(
        &mut self,
        source_collection_id: &str,
        from_index: usize,
        target_collection_id: &str,
        to_index: usize,
    ) -> bool {
        if source_collection_id == target_collection_id {
            return self.reorder_collection(source_collection_id, from_index, to_index);
        }

        let Some(item) = self.remove_item_from_collection(source_collection_id, from_index) else {
            return false;
        };

        if target_collection_id == DOCK_COLLECTION_ID && self.dock.len() >= DOCK_CAPACITY {
            let swap_index = to_index.min(self.dock.len().saturating_sub(1));
            let displaced_item = std::mem::replace(&mut self.dock[swap_index], item);
            let source_len = self.collection_len(source_collection_id).unwrap_or_default();
            let restore_index = from_index.min(source_len);
            if !self.insert_item_into_collection(source_collection_id, restore_index, displaced_item.clone())
            {
                self.dock[swap_index] = displaced_item;
                return false;
            }
            return true;
        }

        if !self.insert_item_into_collection(target_collection_id, to_index, item.clone()) {
            let source_len = self.collection_len(source_collection_id).unwrap_or_default();
            let restore_index = from_index.min(source_len);
            let _ = self.insert_item_into_collection(source_collection_id, restore_index, item);
            return false;
        }

        true
    }

    pub(crate) fn sync_persistent(&self, persistent: &mut PersistentState) {
        persistent.page_orders = self.page_orders();
        persistent.dock_order = self.dock_order();
        persistent.layout_version = LAYOUT_VERSION;
    }

    fn reorder_collection(&mut self, collection_id: &str, from_index: usize, to_index: usize) -> bool {
        if collection_id == DOCK_COLLECTION_ID {
            reorder_items(&mut self.dock, from_index, to_index);
            return true;
        }

        let Some(item) = self.remove_item_from_collection(collection_id, from_index) else {
            return false;
        };

        if self.insert_item_into_collection(collection_id, to_index, item.clone()) {
            true
        } else {
            let _ = self.insert_item_into_collection(collection_id, from_index, item);
            false
        }
    }

    fn collection_len(&self, collection_id: &str) -> Option<usize> {
        if collection_id == DOCK_COLLECTION_ID {
            return Some(self.dock.len());
        }

        let page_index = page_index_from_collection_id(collection_id)?;
        self.pages.get(page_index).map(|_| page_capacity_for(page_index))
    }

    fn remove_item_from_collection(&mut self, collection_id: &str, index: usize) -> Option<LauncherItem> {
        if collection_id == DOCK_COLLECTION_ID {
            return (index < self.dock.len()).then(|| self.dock.remove(index));
        }

        let page_index = page_index_from_collection_id(collection_id)?;
        let page = self.pages.get_mut(page_index)?;
        let item_index = page.items.iter().position(|page_item| page_item.slot == index)?;
        Some(page.items.remove(item_index).item)
    }

    fn insert_item_into_collection(&mut self, collection_id: &str, index: usize, item: LauncherItem) -> bool {
        if collection_id == DOCK_COLLECTION_ID {
            let insert_index = index.min(self.dock.len());
            self.dock.insert(insert_index, item);
            return true;
        }

        if let Some(page_index) =
            page_index_from_collection_id(collection_id).filter(|page_index| *page_index < self.pages.len())
        {
            self.insert_item_into_page(page_index, index, item);
            return true;
        }

        false
    }

    fn insert_item_into_page(&mut self, page_index: usize, index: usize, item: LauncherItem) {
        Self::insert_item_into_pages(&mut self.pages, page_index, index, item);
    }

    fn enforce_page_capacity(pages: &mut Vec<LauncherPage>) {
        let mut page_index = 0;
        while page_index < pages.len() {
            Self::rebalance_page_slots(pages, page_index);
            page_index += 1;
        }
    }

    fn insert_item_into_pages(
        pages: &mut Vec<LauncherPage>,
        mut page_index: usize,
        index: usize,
        mut item: LauncherItem,
    ) {
        let mut slot = index;
        loop {
            while page_index >= pages.len() {
                pages.push(LauncherPage { items: Vec::new() });
            }

            let capacity = page_capacity_for(page_index);
            let target_slot = slot.min(capacity.saturating_sub(1));
            match insert_item_into_page_slot(&mut pages[page_index], capacity, target_slot, item) {
                Some(overflow) => {
                    item = overflow;
                    page_index += 1;
                    slot = 0;
                }
                None => break,
            }
        }
    }

    fn rebalance_page_slots(pages: &mut Vec<LauncherPage>, page_index: usize) {
        let capacity = page_capacity_for(page_index);
        sort_page_items(&mut pages[page_index]);

        let mut seen_slots = BTreeSet::new();
        let mut retained = Vec::new();
        let mut overflow = Vec::new();
        for page_item in pages[page_index].items.drain(..) {
            if page_item.slot < capacity && seen_slots.insert(page_item.slot) {
                retained.push(page_item);
            } else {
                overflow.push(page_item.item);
            }
        }
        pages[page_index].items = retained;

        for item in overflow.into_iter().rev() {
            Self::insert_item_into_pages(pages, page_index + 1, 0, item);
        }
    }

    fn page_orders(&self) -> Vec<Vec<String>> { self.pages.iter().map(page_order).collect() }

    fn dock_order(&self) -> Vec<String> { self.dock.iter().map(|item| item.id.clone()).collect() }
}

pub(crate) fn page_collection_id(page_index: usize) -> String {
    format!("{PAGE_COLLECTION_PREFIX}{page_index}")
}

fn page_capacity_for(page_index: usize) -> usize {
    if page_index == 0 {
        GRAPH_PAGE_CAPACITY
    } else {
        STANDARD_PAGE_CAPACITY
    }
}

fn insert_item_into_page_slot(
    page: &mut LauncherPage,
    capacity: usize,
    slot: usize,
    item: LauncherItem,
) -> Option<LauncherItem> {
    if capacity == 0 {
        return Some(item);
    }

    let mut incoming = LauncherPageItem { slot: slot.min(capacity - 1), item };
    loop {
        if incoming.slot >= capacity {
            sort_page_items(page);
            return Some(incoming.item);
        }

        if let Some(existing_index) = page.items.iter().position(|page_item| page_item.slot == incoming.slot)
        {
            std::mem::swap(&mut page.items[existing_index].item, &mut incoming.item);
            incoming.slot += 1;
        } else {
            page.items.push(incoming);
            sort_page_items(page);
            return None;
        }
    }
}

fn sort_page_items(page: &mut LauncherPage) { page.items.sort_by_key(|page_item| page_item.slot); }

fn first_open_page_slot(page: &LauncherPage, capacity: usize) -> Option<usize> {
    (0..capacity).find(|slot| page.items.iter().all(|page_item| page_item.slot != *slot))
}

fn claim_item(remaining: &mut Vec<LauncherItem>, id: &str) -> Option<LauncherItem> {
    let index = remaining.iter().position(|item| item.id == id)?;
    Some(remaining.remove(index))
}

fn page_index_from_collection_id(collection_id: &str) -> Option<usize> {
    collection_id.strip_prefix(PAGE_COLLECTION_PREFIX).and_then(|suffix| suffix.parse().ok())
}

fn reorder_items(items: &mut Vec<LauncherItem>, from_index: usize, to_index: usize) {
    if from_index >= items.len() || to_index >= items.len() || from_index == to_index {
        return;
    }

    let item = items.remove(from_index);
    items.insert(to_index, item);
}

fn page_order(page: &LauncherPage) -> Vec<String> {
    let mut ids = vec![
        String::new();
        page.items.iter().map(|page_item| page_item.slot).max().map_or(0, |slot| slot + 1)
    ];
    for page_item in &page.items {
        ids[page_item.slot] = page_item.item.id.clone();
    }
    ids
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

    fn item_ids(items: &[LauncherItem]) -> Vec<&str> { items.iter().map(|item| item.id.as_str()).collect() }

    fn page_item_ids(items: &[LauncherPageItem]) -> Vec<&str> {
        items.iter().map(|page_item| page_item.item.id.as_str()).collect()
    }

    fn page_item_slots(items: &[LauncherPageItem]) -> Vec<usize> {
        items.iter().map(|page_item| page_item.slot).collect()
    }

    fn test_page(items: &[&str]) -> LauncherPage {
        LauncherPage {
            items: items
                .iter()
                .enumerate()
                .map(|(slot, id)| LauncherPageItem { slot, item: test_item(id) })
                .collect(),
        }
    }

    fn test_launcher(pages: Vec<LauncherPage>, dock: &[&str]) -> LauncherConfig {
        LauncherConfig { pages, dock: dock.iter().map(|id| test_item(id)).collect() }
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

        assert_eq!(item_ids(&config.dock), [SETTINGS_APP_ID_STR, BITCOIN_APP_ID, SCAN_QR_ACTION_ID]);
        assert_eq!(config.pages.len(), 1);
        assert_eq!(page_item_ids(&config.pages[0].items), ["a", "b"]);
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

        assert_eq!(item_ids(&config.dock), [SETTINGS_APP_ID_STR, BITCOIN_APP_ID, SCAN_QR_ACTION_ID]);
        assert_eq!(page_item_ids(&config.pages[0].items), ["a"]);
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

        assert_eq!(item_ids(&config.dock), [SETTINGS_APP_ID_STR, BITCOIN_APP_ID, SCAN_QR_ACTION_ID]);
        assert_eq!(page_item_ids(&config.pages[0].items), ["a"]);
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

        assert_eq!(item_ids(&config.dock), [BITCOIN_APP_ID, SCAN_QR_ACTION_ID]);
        assert_eq!(page_item_ids(&config.pages[0].items), [SETTINGS_APP_ID_STR]);
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

        assert_eq!(item_ids(&config.dock), ["d1"]);
        assert_eq!(page_item_ids(&config.pages[0].items), ["a", "b", "c", "new"]);
        assert_eq!(page_item_ids(&config.pages[1].items), ["e"]);
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
        assert_eq!(page_item_ids(&config.pages[1].items), ["overflow"]);
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
        assert_eq!(config.pages[1].items.last().map(|page_item| page_item.item.id.as_str()), Some("new"));
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

        assert_eq!(page_item_ids(&config.pages[0].items), ["a", "b", "c", "d", "e", "f"]);
        assert_eq!(page_item_ids(&config.pages[1].items), ["g", "h", "i"]);
    }

    #[test]
    fn moving_item_to_full_page_pushes_last_item_to_next_page() {
        let mut launcher =
            test_launcher(vec![test_page(&["a", "b", "c", "d", "e", "f"]), test_page(&["g", "h"])], &[]);

        assert!(launcher.move_item("page-1", 1, "page-0", 2));

        assert_eq!(page_item_ids(&launcher.pages[0].items), ["a", "b", "h", "c", "d", "e"]);
        assert_eq!(page_item_ids(&launcher.pages[1].items), ["f", "g"]);
    }

    #[test]
    fn moving_item_within_page_to_empty_row_preserves_target_slot() {
        let mut launcher = test_launcher(vec![test_page(&["a", "b"])], &[]);

        assert!(launcher.move_item("page-0", 1, "page-0", 4));

        assert_eq!(page_item_ids(&launcher.pages[0].items), ["a", "b"]);
        assert_eq!(page_item_slots(&launcher.pages[0].items), [0, 4]);
        assert_eq!(
            launcher.page_orders().first(),
            Some(&vec!["a".to_string(), String::new(), String::new(), String::new(), "b".to_string()])
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

        assert_eq!(page_item_ids(&config.pages[0].items), ["a", "b"]);
        assert_eq!(page_item_slots(&config.pages[0].items), [0, 4]);
    }

    #[test]
    fn sparse_page_slots_survive_save_and_refresh_cycle() {
        let mut launcher = test_launcher(vec![test_page(&["a", "b"])], &[]);

        assert!(launcher.move_item("page-0", 1, "page-0", 4));

        let mut persistent = PersistentState::default();
        launcher.sync_persistent(&mut persistent);
        let config = LauncherConfig::from_persistent(&persistent, vec![test_item("a"), test_item("b")]);

        assert_eq!(page_item_ids(&config.pages[0].items), ["a", "b"]);
        assert_eq!(page_item_slots(&config.pages[0].items), [0, 4]);
    }

    #[test]
    fn moving_item_between_pages_to_empty_row_preserves_target_slot() {
        let mut launcher = test_launcher(vec![test_page(&["a", "b"]), test_page(&["x"])], &[]);

        assert!(launcher.move_item("page-0", 1, "page-1", 4));

        assert_eq!(page_item_ids(&launcher.pages[0].items), ["a"]);
        assert_eq!(page_item_slots(&launcher.pages[0].items), [0]);
        assert_eq!(page_item_ids(&launcher.pages[1].items), ["x", "b"]);
        assert_eq!(page_item_slots(&launcher.pages[1].items), [0, 4]);
    }

    #[test]
    fn moving_item_to_full_page_end_slot_still_places_item_on_target_page() {
        let mut launcher = test_launcher(vec![test_page(&["a", "b", "c", "d", "e", "f"])], &["x"]);

        assert!(launcher.move_item("dock", 0, "page-0", GRAPH_PAGE_CAPACITY));

        assert!(launcher.dock.is_empty());
        assert_eq!(page_item_ids(&launcher.pages[0].items), ["a", "b", "c", "d", "e", "x"]);
        assert_eq!(page_item_ids(&launcher.pages[1].items), ["f"]);
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
        assert_eq!(page_item_ids(&launcher.pages[0].items), ["x", "a0", "a1", "a2", "a3", "a4"]);
        assert_eq!(
            page_item_ids(&launcher.pages[1].items),
            ["a5", "b0", "b1", "b2", "b3", "b4", "b5", "b6", "b7"]
        );
        assert_eq!(page_item_ids(&launcher.pages[2].items), ["b8"]);
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

        assert_eq!(item_ids(&config.dock), ["kept"]);
        assert_eq!(config.pages.len(), 1);
        assert_eq!(page_item_ids(&config.pages[0].items), ["still-here"]);
    }
}
